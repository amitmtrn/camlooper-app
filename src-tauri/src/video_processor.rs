use anyhow::{anyhow, Result};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use ffmpeg_next as ffmpeg;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use std::time::{Duration, Instant};
use tauri::{AppHandle, Emitter};
use tokio::sync::{Mutex, RwLock};
use tokio::task::JoinHandle;
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

// Frame buffer structure for smooth streaming
struct FrameBuffer {
    frames: VecDeque<VideoFrame>,
    max_size: usize,
    target_size: usize,
    last_consumed: Instant,
}

impl FrameBuffer {
    fn new(max_size: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(max_size),
            max_size,
            target_size: max_size / 2, // Keep buffer half full ideally
            last_consumed: Instant::now(),
        }
    }

    fn push(&mut self, frame: VideoFrame) -> bool {
        if self.frames.len() >= self.max_size {
            // Buffer is full, drop oldest frame
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
        true
    }

    fn pop(&mut self) -> Option<VideoFrame> {
        self.last_consumed = Instant::now();
        self.frames.pop_front()
    }

    fn len(&self) -> usize {
        self.frames.len()
    }



    fn health_ratio(&self) -> f32 {
        (self.frames.len() as f32 / self.target_size as f32).min(1.0)
    }

    fn should_drop_frame(&self) -> bool {
        self.frames.len() > self.max_size * 3 / 4 // Drop if buffer is 75% full
    }
}

// Adaptive quality controller
struct QualityController {
    current_quality: u8,
    target_fps: f64,
    frame_times: VecDeque<Duration>,
    last_adjustment: Instant,
    adjustment_interval: Duration,
}

impl QualityController {
    fn new(target_fps: f64) -> Self {
        Self {
            current_quality: 80, // Start with higher quality
            target_fps,
            frame_times: VecDeque::with_capacity(10), 
            last_adjustment: Instant::now(),
            adjustment_interval: Duration::from_secs(2), // Less frequent adjustments for stability
        }
    }

    fn record_frame_time(&mut self, time: Duration) {
        if self.frame_times.len() >= 30 {
            self.frame_times.pop_front();
        }
        self.frame_times.push_back(time);
    }

    fn should_adjust_quality(&self) -> bool {
        self.last_adjustment.elapsed() > self.adjustment_interval && 
        self.frame_times.len() >= 10
    }

    fn adjust_quality(&mut self, buffer_health: f32) -> u8 {
        if !self.should_adjust_quality() {
            return self.current_quality;
        }

        let avg_frame_time = self.frame_times.iter().sum::<Duration>() / self.frame_times.len() as u32;
        let target_frame_time = Duration::from_secs_f64(1.0 / self.target_fps);
        
        let performance_ratio = target_frame_time.as_secs_f64() / avg_frame_time.as_secs_f64();
        
        // Adjust quality based on performance and buffer health
        let new_quality = if performance_ratio < 0.8 || buffer_health < 0.3 {
            // Performance is poor or buffer is low, reduce quality
            (self.current_quality as i16 - 10).max(40) as u8
        } else if performance_ratio > 1.2 && buffer_health > 0.7 {
            // Performance is good and buffer is healthy, increase quality
            (self.current_quality as u16 + 5).min(95) as u8
        } else {
            self.current_quality
        };

        if new_quality != self.current_quality {
            self.current_quality = new_quality;
            self.last_adjustment = Instant::now();
        }

        self.current_quality
    }
}

pub struct VideoProcessor {
    video_info: Option<VideoInfo>,
    video_path: Option<PathBuf>,
    stream_status: Arc<RwLock<StreamStatus>>,
    app_handle: Option<AppHandle>,
    is_streaming: Arc<AtomicBool>,
    loop_settings: Arc<RwLock<(u32, bool)>>, // (loop_count, auto_start)
    frame_buffer: Arc<Mutex<FrameBuffer>>,
    quality_controller: Arc<Mutex<QualityController>>,
    performance_metrics: Arc<RwLock<PerformanceMetrics>>,
    streaming_task: Option<JoinHandle<()>>,
    buffer_consumer_task: Option<JoinHandle<()>>,
    frame_sequence: Arc<AtomicU64>,
    batch_size: usize,
}

impl VideoProcessor {
    pub fn new() -> Self {
        // Initialize ffmpeg
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        ffmpeg::init().ok();
        
        Self {
            video_info: None,
            video_path: None,
            stream_status: Arc::new(RwLock::new(StreamStatus {
                is_playing: false,
                current_time: 0.0,
                duration: 0.0,
                loop_count: 5,
                current_loop: 0,
                buffer_health: 0.0,
                actual_fps: 0.0,
                target_fps: 30.0,
                frames_dropped: 0,
                average_processing_time: 0.0,
            })),
            app_handle: None,
            is_streaming: Arc::new(AtomicBool::new(false)),
            loop_settings: Arc::new(RwLock::new((5, true))),
            frame_buffer: Arc::new(Mutex::new(FrameBuffer::new(90))), // 3 seconds at 30fps for better buffering
            quality_controller: Arc::new(Mutex::new(QualityController::new(30.0))),
            performance_metrics: Arc::new(RwLock::new(PerformanceMetrics {
                frames_processed: 0,
                frames_dropped: 0,
                average_encode_time: 0.0,
                average_decode_time: 0.0,
                current_quality: 80, // Start with higher quality
                buffer_size: 0,
                memory_usage: 0,
            })),
            streaming_task: None,
            buffer_consumer_task: None,
            frame_sequence: Arc::new(AtomicU64::new(0)),
            batch_size: 5, // Send 5 frames per batch for better throughput
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
        // Extract all video information synchronously to avoid Send issues
        let video_info = {
            // Open the input file
            let input = ffmpeg::format::input(&file_path)?;
            
            // Find the video stream
            let video_stream = input
                .streams()
                .best(ffmpeg::media::Type::Video)
                .ok_or_else(|| anyhow!("No video stream found"))?;
            
            // Get video information
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

        // Update status and controllers
        {
            let mut status = self.stream_status.write().await;
            status.duration = video_info.duration;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.target_fps = video_info.fps.min(30.0); // Cap at 30fps
            status.buffer_health = 0.0;
            status.frames_dropped = 0;
        }

        // Reset quality controller for new video
        {
            let mut quality_controller = self.quality_controller.lock().await;
            *quality_controller = QualityController::new(video_info.fps.min(30.0));
        }

        // Clear buffer - use larger buffer for better performance
        {
            let mut buffer = self.frame_buffer.lock().await;
            *buffer = FrameBuffer::new(90); // 3 seconds at 30fps
        }

        // Reset metrics
        {
            let mut metrics = self.performance_metrics.write().await;
            *metrics = PerformanceMetrics {
                frames_processed: 0,
                frames_dropped: 0,
                average_encode_time: 0.0,
                average_decode_time: 0.0,
                current_quality: 80, // Start with higher quality
                buffer_size: 0,
                memory_usage: 0,
            };
        }

        self.video_info = Some(video_info.clone());
        self.video_path = Some(file_path);

        Ok(video_info)
    }

    #[cfg(windows)]
    pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
        // For Windows, we'll use a simplified approach without FFmpeg
        // In a real implementation, you could use Windows Media Foundation
        
        // Create a mock video info for Windows (you can enhance this)
        let video_info = VideoInfo {
            id: Uuid::new_v4().to_string(),
            filename: file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            duration: 60.0, // Default 60 seconds
            width: 1920,
            height: 1080,
            fps: 30.0,
            format: "Unknown".to_string(),
        };

        // Update status and controllers
        {
            let mut status = self.stream_status.write().await;
            status.duration = video_info.duration;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.target_fps = video_info.fps.min(30.0);
            status.buffer_health = 0.0;
            status.frames_dropped = 0;
        }

        // Reset quality controller for new video
        {
            let mut quality_controller = self.quality_controller.lock().await;
            *quality_controller = QualityController::new(video_info.fps.min(30.0));
        }

        // Clear buffer
        {
            let mut buffer = self.frame_buffer.lock().await;
            *buffer = FrameBuffer::new(90);
        }

        // Reset metrics
        {
            let mut metrics = self.performance_metrics.write().await;
            *metrics = PerformanceMetrics {
                frames_processed: 0,
                frames_dropped: 0,
                average_encode_time: 0.0,
                average_decode_time: 0.0,
                current_quality: 80,
                buffer_size: 0,
                memory_usage: 0,
            };
        }

        self.video_info = Some(video_info.clone());
        self.video_path = Some(file_path);

        println!("Video loaded on Windows (FFmpeg not available): {}", video_info.filename);
        Ok(video_info)
    }

    pub async fn start_streaming(&mut self) -> Result<()> {
        let video_path = self.video_path.as_ref()
            .ok_or_else(|| anyhow!("No video loaded"))?
            .clone();

        let app_handle = self.app_handle.as_ref()
            .ok_or_else(|| anyhow!("App handle not set"))?
            .clone();

        // Stop any existing streaming
        self.stop_streaming().await?;

        // Set streaming flag
        self.is_streaming.store(true, Ordering::Relaxed);

        // Start frame producer task
        let producer_task = self.start_frame_producer(video_path).await?;
        self.streaming_task = Some(producer_task);

        // Start buffer consumer task
        let consumer_task = self.start_buffer_consumer(app_handle).await?;
        self.buffer_consumer_task = Some(consumer_task);

        // Update status
        {
            let mut status = self.stream_status.write().await;
            status.is_playing = true;
        }

        Ok(())
    }

    async fn start_frame_producer(&self, video_path: PathBuf) -> Result<JoinHandle<()>> {
        let is_streaming = self.is_streaming.clone();
        let loop_settings = self.loop_settings.clone();
        let frame_buffer = self.frame_buffer.clone();
        let quality_controller = self.quality_controller.clone();
        let performance_metrics = self.performance_metrics.clone();
        let stream_status = self.stream_status.clone();

        let task = tokio::task::spawn_blocking(move || {
            if let Err(e) = Self::produce_frames_blocking(
                video_path,
                is_streaming,
                loop_settings,
                frame_buffer,
                quality_controller,
                performance_metrics,
                stream_status,
            ) {
                eprintln!("Error producing frames: {}", e);
            }
        });

        Ok(task)
    }

    async fn start_buffer_consumer(&self, app_handle: AppHandle) -> Result<JoinHandle<()>> {
        let is_streaming = self.is_streaming.clone();
        let frame_buffer = self.frame_buffer.clone();
        let frame_sequence = self.frame_sequence.clone();
        let batch_size = self.batch_size;
        let stream_status = self.stream_status.clone();

        let task = tokio::spawn(async move {
            let mut batch = Vec::with_capacity(batch_size);
            let mut last_emit = Instant::now();
            let emit_interval = Duration::from_millis(16); // ~60fps max emit rate for responsiveness

            while is_streaming.load(Ordering::Relaxed) {
                // Try to fill batch aggressively
                {
                    let mut buffer = frame_buffer.lock().await;
                    while batch.len() < batch_size {
                        if let Some(frame) = buffer.pop() {
                            batch.push(frame);
                        } else {
                            break;
                        }
                    }

                    // Update buffer health only when we have frames
                    if !batch.is_empty() {
                        let buffer_health = buffer.health_ratio();
                        {
                            let mut status = stream_status.write().await;
                            status.buffer_health = buffer_health;
                        }
                    }
                }

                // Emit batch more aggressively - emit immediately if we have frames and enough time passed
                let should_emit = !batch.is_empty() && 
                    (last_emit.elapsed() >= emit_interval || batch.len() >= batch_size);

                if should_emit {
                    let frame_batch = FrameBatch {
                        frames: batch.clone(),
                        sequence_id: frame_sequence.fetch_add(1, Ordering::Relaxed),
                        total_frames: batch.len(),
                    };

                    if let Err(e) = app_handle.emit("video-frame-batch", &frame_batch) {
                        eprintln!("Failed to emit frame batch: {}", e);
                    }

                    batch.clear();
                    last_emit = Instant::now();
                }

                // Even shorter delay for maximum responsiveness
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        });

        Ok(task)
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn produce_frames_blocking(
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        loop_settings: Arc<RwLock<(u32, bool)>>,
        frame_buffer: Arc<Mutex<FrameBuffer>>,
        _quality_controller: Arc<Mutex<QualityController>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        stream_status: Arc<RwLock<StreamStatus>>,
    ) -> Result<()> {
        // For blocking operations, we need to use blocking versions
        // Convert async locks to blocking operations using tokio::runtime::Handle
        let rt = tokio::runtime::Handle::current();
        
        let mut input = ffmpeg::format::input(&video_path)?;
        
        let video_stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| anyhow!("No video stream found"))?;
        
        let video_stream_index = video_stream.index();
        let time_base = f64::from(video_stream.time_base());
        
        let context = ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
        let mut decoder = context.decoder().video()?;
        
        // Calculate proper aspect ratio preserving dimensions
        let (target_width, target_height) = {
            let src_width = decoder.width() as f64;
            let src_height = decoder.height() as f64;
            let src_aspect = src_width / src_height;
            
            // Target resolution constraints
            let max_width = 640.0;
            let max_height = 480.0;
            
            // Calculate dimensions that fit within constraints while preserving aspect ratio
            let (width, height) = if src_width > max_width || src_height > max_height {
                let scale_x = max_width / src_width;
                let scale_y = max_height / src_height;
                let scale = scale_x.min(scale_y);
                
                let new_width = (src_width * scale).round() as u32;
                let new_height = (src_height * scale).round() as u32;
                
                // Ensure dimensions are even (required for many codecs)
                ((new_width / 2) * 2, (new_height / 2) * 2)
            } else {
                // Keep original size if it fits
                (decoder.width(), decoder.height())
            };
            
            println!("Video scaling: {}x{} -> {}x{} (aspect ratio: {:.3})", 
                     decoder.width(), decoder.height(), width, height, src_aspect);
            
            (width, height)
        };

        // Create scaler with proper quality and color space handling
        let mut scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGB24,
            target_width,
            target_height,
            ffmpeg::software::scaling::Flags::LANCZOS, // Better quality scaling
        )?;
        
        let mut frame = ffmpeg::util::frame::video::Video::empty();
        let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
        
        let fps = f64::from(video_stream.avg_frame_rate());
        let target_fps = fps.min(30.0);
        let frame_duration = Duration::from_nanos(1_000_000_000 / target_fps as u64);
        
        let mut frame_count = 0u64;
        let mut frames_dropped = 0u32;
        let mut last_fps_update = Instant::now();
        let mut fps_frame_count = 0u64;
        let mut current_quality = 80u8; // Start with higher quality for better visual quality
        let mut frame_times = VecDeque::with_capacity(10);
        let mut last_buffer_check = Instant::now();

        // Remove frame skipping to avoid temporal artifacts
        // Process all frames for better quality
        
        loop {
            // Check if we should continue streaming
            if !is_streaming.load(Ordering::Relaxed) {
                break;
            }

            let (loop_count, _auto_start) = {
                let settings = rt.block_on(async { loop_settings.read().await });
                *settings
            };

            // Process packets
            for (stream, packet) in input.packets() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    
                    while decoder.receive_frame(&mut frame).is_ok() {
                        let frame_start = Instant::now();
                        frame_count += 1;
                        fps_frame_count += 1;
                        
                        // Fast buffer health check - only check every 10th frame
                        let should_drop = if frame_count % 10 == 0 && last_buffer_check.elapsed() > Duration::from_millis(100) {
                            let buffer = rt.block_on(async { frame_buffer.lock().await });
                            last_buffer_check = Instant::now();
                            buffer.should_drop_frame()
                        } else {
                            false
                        };

                        if should_drop {
                            frames_dropped += 1;
                            continue;
                        }
                        
                        // Scale frame to RGB24 (downscaled for better performance)
                        scaler.run(&frame, &mut rgb_frame)?;
                        
                        // More conservative adaptive quality control
                        if frame_times.len() >= 10 {
                            let avg_time = frame_times.iter().sum::<Duration>() / frame_times.len() as u32;
                            if avg_time > Duration::from_millis(33) { // If taking more than 33ms per frame (slower than 30fps)
                                current_quality = (current_quality as i16 - 3).max(60) as u8; // Don't go below 60 quality
                            } else if avg_time < Duration::from_millis(15) && current_quality < 90 {
                                current_quality = (current_quality as u16 + 1).min(90) as u8; // Allow up to 90 quality
                            }
                        }
                        
                        // Convert to JPEG with aggressive optimization
                        let jpeg_data = Self::rgb_frame_to_jpeg_fast(&rgb_frame, current_quality)?;
                        let timestamp = frame.timestamp().unwrap_or(0) as f64 * time_base;
                        
                        // Create video frame
                        let video_frame = VideoFrame {
                            data: jpeg_data,
                            timestamp,
                            width: rgb_frame.width(),
                            height: rgb_frame.height(),
                        };

                        // Push frame to buffer
                        {
                            let mut buffer = rt.block_on(async { frame_buffer.lock().await });
                            buffer.push(video_frame);
                        }

                        // Record frame time for performance tracking
                        let processing_time = frame_start.elapsed();
                        if frame_times.len() >= 10 {
                            frame_times.pop_front();
                        }
                        frame_times.push_back(processing_time);

                        // Update status every 15 frames instead of every 10
                        if frame_count % 15 == 0 {
                            let mut status = rt.block_on(async { stream_status.write().await });
                            status.current_time = timestamp;
                            status.frames_dropped = frames_dropped;
                            status.average_processing_time = frame_times.iter().sum::<Duration>().as_secs_f64() / frame_times.len() as f64;
                        }

                        // Update FPS every second
                        if last_fps_update.elapsed() >= Duration::from_secs(1) {
                            let elapsed = last_fps_update.elapsed().as_secs_f64();
                            let actual_fps = fps_frame_count as f64 / elapsed;
                            
                            let mut status = rt.block_on(async { stream_status.write().await });
                            status.actual_fps = actual_fps;
                            
                            last_fps_update = Instant::now();
                            fps_frame_count = 0;
                        }

                        // Minimal frame rate control - only sleep if we're way too fast
                        if processing_time < frame_duration {
                            let sleep_time = frame_duration - processing_time;
                            // Only sleep if the sleep time is very significant (> 10ms)
                            if sleep_time > Duration::from_millis(10) {
                                std::thread::sleep(sleep_time / 4); // Sleep for quarter the time
                            }
                        }

                        // Check if we should stop less frequently
                        if frame_count % 30 == 0 && !is_streaming.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                    }
                }
            }

            // Handle looping
            {
                let mut status = rt.block_on(async { stream_status.write().await });
                status.current_loop += 1;
                
                if loop_count == 10 || status.current_loop < loop_count {
                    // Continue looping
                    status.current_time = 0.0;
                    input.seek(0, 0..i64::MAX)?;
                } else {
                    // Stop looping
                    status.is_playing = false;
                    break;
                }
            }
        }

        // Update final metrics
        {
            let mut metrics = rt.block_on(async { performance_metrics.write().await });
            metrics.frames_processed = frame_count;
            metrics.frames_dropped = frames_dropped as u64;
            metrics.current_quality = current_quality;
        }

        is_streaming.store(false, Ordering::Relaxed);
        Ok(())
    }

    #[cfg(windows)]
    fn produce_frames_blocking(
        video_path: PathBuf,
        is_streaming: Arc<AtomicBool>,
        loop_settings: Arc<RwLock<(u32, bool)>>,
        frame_buffer: Arc<Mutex<FrameBuffer>>,
        _quality_controller: Arc<Mutex<QualityController>>,
        performance_metrics: Arc<RwLock<PerformanceMetrics>>,
        stream_status: Arc<RwLock<StreamStatus>>,
    ) -> Result<()> {
        println!("Video processing on Windows (simplified mode without FFmpeg)");
        
        // Simulate frame processing for Windows
        let frame_count = Arc::new(AtomicU64::new(0));
        
        std::thread::spawn(move || {
            while is_streaming.load(Ordering::Relaxed) {
                // Generate a test frame (black frame)
                let test_frame = vec![0u8; 640 * 480 * 3]; // RGB24 black frame
                
                // Convert to JPEG for consistency with other platforms
                if let Ok(jpeg_data) = Self::rgb_to_jpeg_fast(&test_frame, 640, 480, 80) {
                    // Try to send to frame buffer
                    if let Ok(mut buffer) = frame_buffer.try_lock() {
                        let video_frame = crate::video_processor::VideoFrame {
                            data: jpeg_data,
                            timestamp: frame_count.load(Ordering::Relaxed) as f64 / 30.0,
                            width: 640,
                            height: 480,
                        };
                        buffer.push(video_frame);
                    }
                }
                
                frame_count.fetch_add(1, Ordering::Relaxed);
                
                // 30 FPS timing
                std::thread::sleep(std::time::Duration::from_millis(33));
            }
        });
        
        Ok(())
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn rgb_frame_to_jpeg_fast(frame: &ffmpeg::util::frame::video::Video, quality: u8) -> Result<Vec<u8>> {
        use image::{ImageBuffer, Rgb};
        
        let width = frame.width() as u32;
        let height = frame.height() as u32;
        let data = frame.data(0);
        let stride = frame.stride(0);
        
        // Create image buffer from frame data
        let mut img_data = Vec::with_capacity((width * height * 3) as usize);
        
        for y in 0..height {
            let row_start = (y as usize) * stride;
            let row_end = row_start + (width as usize * 3);
            if row_end <= data.len() {
                img_data.extend_from_slice(&data[row_start..row_start + (width as usize * 3)]);
            }
        }
        
        if let Ok(img_buffer) = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, img_data) {
            let mut jpeg_data = Vec::new();
            if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality)
                .encode_image(&img_buffer).is_ok() {
                return Ok(jpeg_data);
            }
        }
        
        Err(anyhow!("Failed to encode frame as JPEG"))
    }

    #[cfg(windows)]
    fn rgb_to_jpeg_fast(rgb_data: &[u8], width: u32, height: u32, quality: u8) -> Result<Vec<u8>> {
        use image::{ImageBuffer, Rgb};
        
        if let Ok(img_buffer) = ImageBuffer::<Rgb<u8>, _>::from_raw(width, height, rgb_data.to_vec()) {
            let mut jpeg_data = Vec::new();
            if image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_data, quality)
                .encode_image(&img_buffer).is_ok() {
                return Ok(jpeg_data);
            }
        }
        
        Err(anyhow!("Failed to encode RGB as JPEG"))
    }



    pub async fn pause_streaming(&mut self) -> Result<()> {
        self.is_streaming.store(false, Ordering::Relaxed);

        // Wait for tasks to finish
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }
        if let Some(task) = self.buffer_consumer_task.take() {
            task.abort();
        }

        {
            let mut status = self.stream_status.write().await;
            status.is_playing = false;
        }

        Ok(())
    }

    pub async fn stop_streaming(&mut self) -> Result<()> {
        self.is_streaming.store(false, Ordering::Relaxed);

        // Wait for tasks to finish
        if let Some(task) = self.streaming_task.take() {
            task.abort();
        }
        if let Some(task) = self.buffer_consumer_task.take() {
            task.abort();
        }

        // Clear buffer - use larger buffer for better performance
        {
            let mut buffer = self.frame_buffer.lock().await;
            *buffer = FrameBuffer::new(90); // 3 seconds at 30fps
        }

        {
            let mut status = self.stream_status.write().await;
            status.is_playing = false;
            status.current_time = 0.0;
            status.current_loop = 0;
            status.buffer_health = 0.0;
            status.frames_dropped = 0;
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
        let mut metrics = self.performance_metrics.read().await.clone();
        
        // Update buffer size
        {
            let buffer = self.frame_buffer.lock().await;
            metrics.buffer_size = buffer.len();
        }

        // Update current quality
        {
            let quality_controller = self.quality_controller.lock().await;
            metrics.current_quality = quality_controller.current_quality;
        }

        metrics
    }
}

// Global video processor instance
static VIDEO_PROCESSOR: Lazy<Arc<Mutex<VideoProcessor>>> = 
    Lazy::new(|| Arc::new(Mutex::new(VideoProcessor::new())));

// Helper functions for Tauri commands
pub async fn load_video_file(file_path: String) -> Result<VideoInfo> {
    let path = PathBuf::from(file_path);
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.load_video(path).await
}

pub async fn start_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.start_streaming().await?;
    
    Ok(())
}

pub async fn pause_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.pause_streaming().await
}

pub async fn stop_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.stop_streaming().await?;
    
    Ok(())
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

// Initialize video processor with app handle for event emission
pub async fn init_video_processor(app_handle: AppHandle) {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.set_app_handle(app_handle);
} 