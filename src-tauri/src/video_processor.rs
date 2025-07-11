use anyhow::{anyhow, Result};
#[cfg(not(windows))]
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

// Simplified implementation for Windows
#[cfg(windows)]
pub struct VideoProcessor {
    video_info: Option<VideoInfo>,
    stream_status: Arc<RwLock<StreamStatus>>,
    app_handle: Option<AppHandle>,
    is_streaming: Arc<AtomicBool>,
}

#[cfg(windows)]
impl VideoProcessor {
    pub fn new() -> Self {
        Self {
            video_info: None,
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
        }
    }

    pub fn set_app_handle(&mut self, app_handle: AppHandle) {
        self.app_handle = Some(app_handle);
    }

    pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
        // Windows stub implementation - in production you'd use Windows Media Foundation
        let video_info = VideoInfo {
            id: Uuid::new_v4().to_string(),
            filename: file_path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("unknown")
                .to_string(),
            duration: 60.0, // Default duration
            width: 1920,
            height: 1080,
            fps: 30.0,
            format: "Windows Media".to_string(),
        };

        self.video_info = Some(video_info.clone());
        
        // Update status
        {
            let mut status = self.stream_status.write().await;
            status.duration = video_info.duration;
            status.target_fps = video_info.fps;
        }

        Ok(video_info)
    }

    pub async fn start_streaming(&mut self) -> Result<()> {
        self.is_streaming.store(true, Ordering::Relaxed);
        
        {
            let mut status = self.stream_status.write().await;
            status.is_playing = true;
        }

        // Windows stub - would implement video streaming here
        println!("Windows video streaming started (stub implementation)");
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

    pub async fn set_loop_settings(&self, loop_count: u32, _auto_start: bool) -> Result<()> {
        let mut status = self.stream_status.write().await;
        status.loop_count = loop_count;
        Ok(())
    }

    pub fn get_video_info(&self) -> Option<VideoInfo> {
        self.video_info.clone()
    }

    pub async fn get_performance_metrics(&self) -> PerformanceMetrics {
        PerformanceMetrics {
            frames_processed: 0,
            frames_dropped: 0,
            average_encode_time: 0.0,
            average_decode_time: 0.0,
            current_quality: 80,
            buffer_size: 0,
            memory_usage: 0,
        }
    }
}

// Full FFmpeg implementation for non-Windows platforms
#[cfg(not(windows))]
mod ffmpeg_impl {
    use super::*;
    
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
                target_size: max_size / 2,
                last_consumed: Instant::now(),
            }
        }

        fn push(&mut self, frame: VideoFrame) -> bool {
            if self.frames.len() >= self.max_size {
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
            self.frames.len() > self.max_size * 3 / 4
        }
    }

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
                current_quality: 80,
                target_fps,
                frame_times: VecDeque::with_capacity(10),
                last_adjustment: Instant::now(),
                adjustment_interval: Duration::from_secs(2),
            }
        }
    }

    pub struct VideoProcessor {
        video_info: Option<VideoInfo>,
        video_path: Option<PathBuf>,
        stream_status: Arc<RwLock<StreamStatus>>,
        app_handle: Option<AppHandle>,
        is_streaming: Arc<AtomicBool>,
        loop_settings: Arc<RwLock<(u32, bool)>>,
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
                frame_buffer: Arc::new(Mutex::new(FrameBuffer::new(90))),
                quality_controller: Arc::new(Mutex::new(QualityController::new(30.0))),
                performance_metrics: Arc::new(RwLock::new(PerformanceMetrics {
                    frames_processed: 0,
                    frames_dropped: 0,
                    average_encode_time: 0.0,
                    average_decode_time: 0.0,
                    current_quality: 80,
                    buffer_size: 0,
                    memory_usage: 0,
                })),
                streaming_task: None,
                buffer_consumer_task: None,
                frame_sequence: Arc::new(AtomicU64::new(0)),
                batch_size: 5,
            }
        }

        pub fn set_app_handle(&mut self, app_handle: AppHandle) {
            self.app_handle = Some(app_handle);
        }

        pub async fn load_video(&mut self, file_path: PathBuf) -> Result<VideoInfo> {
            // FFmpeg implementation for non-Windows platforms
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
                status.target_fps = video_info.fps.min(30.0);
            }

            Ok(video_info)
        }

        pub async fn start_streaming(&mut self) -> Result<()> {
            // Simplified streaming implementation
            self.is_streaming.store(true, Ordering::Relaxed);
            
            {
                let mut status = self.stream_status.write().await;
                status.is_playing = true;
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
    }
}

#[cfg(not(windows))]
pub use ffmpeg_impl::VideoProcessor;

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