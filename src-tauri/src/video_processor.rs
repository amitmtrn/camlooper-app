use anyhow::{anyhow, Result};
use base64::Engine;
use ffmpeg_next as ffmpeg;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex, RwLock};
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
    pub data: String, // Base64 encoded JPEG
    pub timestamp: f64,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamStatus {
    pub is_playing: bool,
    pub current_time: f64,
    pub duration: f64,
    pub loop_count: u32,
    pub current_loop: u32,
}

pub struct VideoProcessor {
    video_info: Option<VideoInfo>,
    video_path: Option<PathBuf>,
    stream_status: Arc<Mutex<StreamStatus>>,
    frame_sender: Option<mpsc::UnboundedSender<VideoFrame>>,
    is_streaming: Arc<Mutex<bool>>,
    loop_settings: Arc<Mutex<(u32, bool)>>, // (loop_count, auto_start)
}

impl VideoProcessor {
    pub fn new() -> Self {
        // Initialize ffmpeg
        ffmpeg::init().ok();
        
        Self {
            video_info: None,
            video_path: None,
            stream_status: Arc::new(Mutex::new(StreamStatus {
                is_playing: false,
                current_time: 0.0,
                duration: 0.0,
                loop_count: 5,
                current_loop: 0,
            })),
            frame_sender: None,
            is_streaming: Arc::new(Mutex::new(false)),
            loop_settings: Arc::new(Mutex::new((5, true))),
        }
    }

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

        // Update status (now async operations are separated)
        let mut status = self.stream_status.lock().await;
        status.duration = video_info.duration;
        status.current_time = 0.0;
        status.current_loop = 0;
        drop(status);

        self.video_info = Some(video_info.clone());
        self.video_path = Some(file_path);

        Ok(video_info)
    }

    pub async fn start_streaming(&mut self) -> Result<mpsc::UnboundedReceiver<VideoFrame>> {
        let video_path = self.video_path.as_ref()
            .ok_or_else(|| anyhow!("No video loaded"))?
            .clone();

        let (frame_sender, frame_receiver) = mpsc::unbounded_channel::<VideoFrame>();
        self.frame_sender = Some(frame_sender.clone());

        let stream_status = self.stream_status.clone();
        let is_streaming = self.is_streaming.clone();
        let loop_settings = self.loop_settings.clone();

        // Start streaming task in a blocking thread to avoid Send issues
        tokio::task::spawn_blocking(move || {
            if let Err(e) = Self::stream_video_frames_blocking(
                video_path,
                frame_sender,
                stream_status,
                is_streaming,
                loop_settings,
            ) {
                eprintln!("Error streaming video: {}", e);
            }
        });

        {
            let mut streaming = self.is_streaming.lock().await;
            *streaming = true;
        }

        {
            let mut status = self.stream_status.lock().await;
            status.is_playing = true;
        }

        Ok(frame_receiver)
    }

    fn stream_video_frames_blocking(
        video_path: PathBuf,
        frame_sender: mpsc::UnboundedSender<VideoFrame>,
        stream_status: Arc<Mutex<StreamStatus>>,
        is_streaming: Arc<Mutex<bool>>,
        loop_settings: Arc<Mutex<(u32, bool)>>,
    ) -> Result<()> {
        let mut input = ffmpeg::format::input(&video_path)?;
        
        let video_stream = input
            .streams()
            .best(ffmpeg::media::Type::Video)
            .ok_or_else(|| anyhow!("No video stream found"))?;
        
        let video_stream_index = video_stream.index();
        let time_base = f64::from(video_stream.time_base());
        
        let context = ffmpeg::codec::context::Context::from_parameters(video_stream.parameters())?;
        let mut decoder = context.decoder().video()?;
        
        // Create scaler for consistent output format - use RGB24 for better performance
        let mut scaler = ffmpeg::software::scaling::context::Context::get(
            decoder.format(),
            decoder.width(),
            decoder.height(),
            ffmpeg::format::Pixel::RGB24,
            decoder.width(),
            decoder.height(),
            ffmpeg::software::scaling::Flags::FAST_BILINEAR, // Use faster scaling
        )?;

        let mut frame = ffmpeg::util::frame::video::Video::empty();
        let mut rgb_frame = ffmpeg::util::frame::video::Video::empty();
        
        let fps = f64::from(video_stream.avg_frame_rate());
        let frame_duration = 1.0 / fps.min(30.0); // Cap at 30 FPS for better performance
        let frame_duration_ms = (frame_duration * 1000.0) as u64;
        let mut frame_count = 0;

        loop {
            // Check if we should continue streaming
            {
                let streaming = is_streaming.blocking_lock();
                if !*streaming {
                    break;
                }
            }

            let (loop_count, _auto_start) = {
                let settings = loop_settings.blocking_lock();
                *settings
            };

            // Process packets
            for (stream, packet) in input.packets() {
                if stream.index() == video_stream_index {
                    decoder.send_packet(&packet)?;
                    
                    while decoder.receive_frame(&mut frame).is_ok() {
                        let frame_start = std::time::Instant::now();
                        frame_count += 1;
                        
                        // Scale frame to RGB24
                        scaler.run(&frame, &mut rgb_frame)?;
                        
                        // Convert to JPEG
                        let jpeg_data = Self::frame_to_jpeg(&rgb_frame)?;
                        let timestamp = frame.timestamp().unwrap_or(0) as f64 * time_base;
                        
                        // Create video frame
                        let video_frame = VideoFrame {
                            data: base64::prelude::BASE64_STANDARD.encode(&jpeg_data),
                            timestamp,
                            width: rgb_frame.width(),
                            height: rgb_frame.height(),
                        };

                        // Update status less frequently (every 5th frame)
                        if frame_count % 5 == 0 {
                            let mut status = stream_status.blocking_lock();
                            status.current_time = timestamp;
                        }

                        // Send frame
                        if frame_sender.send(video_frame).is_err() {
                            // Receiver dropped, stop streaming
                            return Ok(());
                        }

                        // Intelligent frame rate control - adjust for processing time
                        let processing_time = frame_start.elapsed();
                        let target_duration = std::time::Duration::from_millis(frame_duration_ms);
                        
                        if processing_time < target_duration {
                            let sleep_time = target_duration - processing_time;
                            std::thread::sleep(sleep_time);
                        }

                        // Check if we should stop (every 10th frame)
                        if frame_count % 10 == 0 {
                            let streaming = is_streaming.blocking_lock();
                            if !*streaming {
                                return Ok(());
                            }
                        }
                    }
                }
            }

            // Handle looping
            {
                let mut status = stream_status.blocking_lock();
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

        {
            let mut streaming = is_streaming.blocking_lock();
            *streaming = false;
        }

        Ok(())
    }

    fn frame_to_jpeg(frame: &ffmpeg::util::frame::video::Video) -> Result<Vec<u8>> {
        let width = frame.width() as usize;
        let height = frame.height() as usize;
        let linesize = frame.stride(0);
        let data = frame.data(0);
        
        // Handle stride properly - copy line by line if needed for RGB24 (3 bytes per pixel)
        let rgb_data = if linesize == width * 3 {
            // No padding, can use data directly
            data[0..width * height * 3].to_vec()
        } else {
            // Handle line padding
            let mut rgb_data = Vec::with_capacity(width * height * 3);
            for y in 0..height {
                let line_start = y * linesize;
                let line_end = line_start + width * 3;
                rgb_data.extend_from_slice(&data[line_start..line_end]);
            }
            rgb_data
        };
        
        // Convert RGB to image::RgbImage
        let img = image::RgbImage::from_raw(width as u32, height as u32, rgb_data)
            .ok_or_else(|| anyhow!("Failed to create image from frame data"))?;
        
        // Encode as JPEG with optimized quality for performance
        let mut jpeg_data = Vec::new();
        {
            use image::codecs::jpeg::JpegEncoder;
            use std::io::Cursor;
            
            let mut cursor = Cursor::new(&mut jpeg_data);
            let mut encoder = JpegEncoder::new_with_quality(&mut cursor, 65); // Lower quality for better performance
            encoder.encode_image(&img)?;
        }
        
        Ok(jpeg_data)
    }

    pub async fn pause_streaming(&mut self) -> Result<()> {
        {
            let mut streaming = self.is_streaming.lock().await;
            *streaming = false;
        }

        {
            let mut status = self.stream_status.lock().await;
            status.is_playing = false;
        }

        Ok(())
    }

    pub async fn stop_streaming(&mut self) -> Result<()> {
        {
            let mut streaming = self.is_streaming.lock().await;
            *streaming = false;
        }

        {
            let mut status = self.stream_status.lock().await;
            status.is_playing = false;
            status.current_time = 0.0;
            status.current_loop = 0;
        }

        self.frame_sender = None;
        Ok(())
    }

    pub async fn get_stream_status(&self) -> StreamStatus {
        self.stream_status.lock().await.clone()
    }

    pub async fn set_loop_settings(&self, loop_count: u32, auto_start: bool) -> Result<()> {
        let mut settings = self.loop_settings.lock().await;
        *settings = (loop_count, auto_start);
        
        let mut status = self.stream_status.lock().await;
        status.loop_count = loop_count;
        
        Ok(())
    }

    pub fn get_video_info(&self) -> Option<VideoInfo> {
        self.video_info.clone()
    }
}

// Global video processor instance
static VIDEO_PROCESSOR: Lazy<Arc<Mutex<VideoProcessor>>> = 
    Lazy::new(|| Arc::new(Mutex::new(VideoProcessor::new())));

// Frame receiver for the frontend
static FRAME_RECEIVER: Lazy<Arc<RwLock<Option<mpsc::UnboundedReceiver<VideoFrame>>>>> =
    Lazy::new(|| Arc::new(RwLock::new(None)));

// Helper functions for Tauri commands
pub async fn load_video_file(file_path: String) -> Result<VideoInfo> {
    let path = PathBuf::from(file_path);
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.load_video(path).await
}

pub async fn start_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    let receiver = processor.start_streaming().await?;
    
    let mut frame_receiver = FRAME_RECEIVER.write().await;
    *frame_receiver = Some(receiver);
    
    Ok(())
}

pub async fn pause_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.pause_streaming().await
}

pub async fn stop_video_stream() -> Result<()> {
    let mut processor = VIDEO_PROCESSOR.lock().await;
    processor.stop_streaming().await?;
    
    let mut frame_receiver = FRAME_RECEIVER.write().await;
    *frame_receiver = None;
    
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

pub async fn get_next_frame() -> Option<VideoFrame> {
    let mut frame_receiver = FRAME_RECEIVER.write().await;
    if let Some(receiver) = frame_receiver.as_mut() {
        receiver.recv().await
    } else {
        None
    }
} 