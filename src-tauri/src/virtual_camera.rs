use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

#[cfg(target_os = "linux")]
use v4l::Device;
#[cfg(target_os = "linux")]
use v4l::capability;

#[cfg(target_os = "linux")]
use std::process::Stdio;
#[cfg(target_os = "linux")]
use tokio::io::AsyncWriteExt;
#[cfg(target_os = "linux")]
use image;

#[cfg(windows)]
use crate::windows_vcam;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualCameraConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub camera_name: String,
}

impl Default for VirtualCameraConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 30,
            camera_name: "CamLooper Virtual Camera".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VirtualCameraStatus {
    pub is_active: bool,
    pub camera_name: String,
    pub resolution: String,
    pub fps: u32,
    pub frame_count: u64,
}

pub struct VirtualCamera {
    config: VirtualCameraConfig,
    status: Arc<Mutex<VirtualCameraStatus>>,
    frame_sender: Option<mpsc::UnboundedSender<Vec<u8>>>,
    is_running: Arc<Mutex<bool>>,
    #[cfg(windows)]
    windows_vcam: Option<windows_vcam::WindowsVirtualCamera>,
}

impl VirtualCamera {
    pub fn new(config: VirtualCameraConfig) -> Self {
        let status = VirtualCameraStatus {
            is_active: false,
            camera_name: config.camera_name.clone(),
            resolution: format!("{}x{}", config.width, config.height),
            fps: config.fps,
            frame_count: 0,
        };

        Self {
            config,
            status: Arc::new(Mutex::new(status)),
            frame_sender: None,
            is_running: Arc::new(Mutex::new(false)),
            #[cfg(windows)]
            windows_vcam: None,
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        let mut is_running = self.is_running.lock().await;
        if *is_running {
            return Ok(());
        }

        // Create frame channel
        let (frame_sender, frame_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        self.frame_sender = Some(frame_sender);

        // Start platform-specific virtual camera
        self.start_platform_camera(frame_receiver).await?;

        // Update status
        let mut status = self.status.lock().await;
        status.is_active = true;
        status.frame_count = 0;

        *is_running = true;
        println!("Virtual camera started: {}", self.config.camera_name);
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        let mut is_running = self.is_running.lock().await;
        if !*is_running {
            return Ok(());
        }

        // Stop frame sender
        self.frame_sender = None;

        // Stop platform-specific virtual camera
        self.stop_platform_camera().await?;

        // Update status
        let mut status = self.status.lock().await;
        status.is_active = false;

        *is_running = false;
        println!("Virtual camera stopped: {}", self.config.camera_name);
        Ok(())
    }

    pub async fn get_status(&self) -> VirtualCameraStatus {
        self.status.lock().await.clone()
    }

    pub async fn send_frame(&self, frame_data: Vec<u8>) -> Result<()> {
        if let Some(sender) = &self.frame_sender {
            sender.send(frame_data)?;
            
            // Update frame count
            let mut status = self.status.lock().await;
            status.frame_count += 1;
        }
        Ok(())
    }

    pub async fn is_active(&self) -> bool {
        self.status.lock().await.is_active
    }

    #[cfg(target_os = "windows")]
    async fn start_platform_camera(&mut self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Windows DirectShow implementation
        let config = self.config.clone();
        let status = self.status.clone();
        
        // Initialize Windows virtual camera
        let mut vcam = windows_vcam::WindowsVirtualCamera::new(
            &config.camera_name,
            config.width,
            config.height,
            config.fps,
        )?;
        
        vcam.start()?;
        self.windows_vcam = Some(vcam);
        
        tokio::spawn(async move {
            println!("Starting Windows DirectShow virtual camera...");
            
            let mut frame_count = 0u64;
            let target_frame_duration = tokio::time::Duration::from_millis(1000 / config.fps as u64);
            let mut frame_timer = tokio::time::interval(target_frame_duration);
            frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            // Buffer for latest frame
            let mut latest_frame: Option<Vec<u8>> = None;
            
            loop {
                // Wait for the next frame time
                frame_timer.tick().await;
                
                // Collect any available frames (non-blocking)
                while let Ok(frame_data) = frame_receiver.try_recv() {
                    latest_frame = Some(frame_data);
                }
                
                // Send frame to DirectShow
                if let Some(ref frame_data) = latest_frame {
                    // Convert JPEG to RGB if needed
                    if let Ok((rgb_data, width, height)) = Self::jpeg_to_rgb(frame_data) {
                        // Send to Windows virtual camera
                        if let Err(e) = windows_vcam::send_frame_to_virtual_camera(&rgb_data, width, height) {
                            eprintln!("Failed to send frame to Windows virtual camera: {}", e);
                        }
                    }
                } else {
                    // Send black frame if no data available
                    let black_frame = vec![0u8; (config.width * config.height * 3) as usize];
                    if let Err(e) = windows_vcam::send_frame_to_virtual_camera(&black_frame, config.width, config.height) {
                        eprintln!("Failed to send black frame to Windows virtual camera: {}", e);
                    }
                }
                
                frame_count += 1;
                if frame_count % 30 == 0 {
                    println!("Sent {} frames to Windows virtual camera", frame_count);
                }
                
                // Update status
                {
                    let mut status_lock = status.lock().await;
                    status_lock.frame_count = frame_count;
                }
            }
        });
        
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Linux v4l2loopback implementation using FFmpeg with frame rate control
        let _config = self.config.clone();
        let status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting Linux v4l2loopback virtual camera...");
            
            // Try to find an available v4l2loopback device
            let (device_path, _device_index) = match Self::find_loopback_device() {
                Ok((path, index)) => (path, index),
                Err(e) => {
                    eprintln!("Failed to find v4l2loopback device: {}", e);
                    eprintln!("Make sure v4l2loopback is installed and loaded:");
                    eprintln!("  sudo modprobe v4l2loopback");
                    return;
                }
            };
            
            println!("Using v4l2loopback device: {}", device_path);
            
            // Start FFmpeg process to write to the v4l2loopback device
            let mut ffmpeg_process = match tokio::process::Command::new("ffmpeg")
                .args([
                    "-f", "rawvideo",
                    "-pixel_format", "rgb24",
                    "-video_size", "640x480",
                    "-framerate", "30",
                    "-i", "pipe:0",
                    "-f", "v4l2",
                    "-pix_fmt", "rgb24",
                    "-vf", "fps=30",  // Force consistent 30 FPS output
                    &device_path,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn() {
                Ok(process) => process,
                Err(e) => {
                    eprintln!("Failed to start FFmpeg process: {}", e);
                    eprintln!("Make sure FFmpeg is installed: sudo apt install ffmpeg");
                    return;
                }
            };
            
            // Get the stdin handle
            let mut stdin = match ffmpeg_process.stdin.take() {
                Some(stdin) => stdin,
                None => {
                    eprintln!("Failed to get FFmpeg stdin");
                    return;
                }
            };
            
            let mut frame_count = 0u64;
            let mut last_frame_rgb: Option<Vec<u8>> = None;
            let target_frame_duration = tokio::time::Duration::from_millis(33); // 30 FPS = 33.33ms per frame
            let mut frame_timer = tokio::time::interval(target_frame_duration);
            frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            // Create a frame buffer to handle incoming frames
            let mut frame_buffer = std::collections::VecDeque::new();
            const MAX_BUFFER_SIZE: usize = 10;
            
            // Spawn a task to collect incoming frames
            let (buffer_sender, mut buffer_receiver) = tokio::sync::mpsc::unbounded_channel();
            tokio::spawn(async move {
                while let Some(frame_data) = frame_receiver.recv().await {
                    if buffer_sender.send(frame_data).is_err() {
                        break;
                    }
                }
            });
            
            println!("Virtual camera frame processing started");
            
            loop {
                // Wait for the next frame time
                frame_timer.tick().await;
                
                // Collect any available frames (non-blocking)
                while let Ok(frame_data) = buffer_receiver.try_recv() {
                    if frame_buffer.len() >= MAX_BUFFER_SIZE {
                        // Drop oldest frame if buffer is full
                        frame_buffer.pop_front();
                    }
                    frame_buffer.push_back(frame_data);
                }
                
                // Get the most recent frame to send
                let current_frame_data = if let Some(frame_data) = frame_buffer.pop_back() {
                    // Clear any remaining buffered frames to use the latest
                    frame_buffer.clear();
                    Some(frame_data)
                } else {
                    None
                };
                
                // Convert frame data to RGB if available
                let (rgb_data, frame_width, frame_height) = if let Some(frame_data) = current_frame_data {
                    match Self::jpeg_to_raw_rgb(&frame_data) {
                        Ok((data, width, height)) => {
                            // Cache this frame for frame repetition  
                            last_frame_rgb = Some(data.clone());
                            (Some(data), width, height)
                        }
                        Err(e) => {
                            eprintln!("Failed to convert JPEG to RGB: {}", e);
                            // Use last known good frame with default dimensions
                            if let Some(ref cached_frame) = last_frame_rgb {
                                (Some(cached_frame.clone()), 640, 480)
                            } else {
                                (None, 640, 480)
                            }
                        }
                    }
                } else {
                    // No new frame available, repeat last frame
                    if let Some(ref cached_frame) = last_frame_rgb {
                        (Some(cached_frame.clone()), 640, 480)
                    } else {
                        (None, 640, 480)
                    }
                };
                
                // Send frame to FFmpeg (or black frame if no data available)
                let final_rgb_data = if let Some(rgb_data) = rgb_data {
                    // Scale to target resolution if needed
                    if (frame_width, frame_height) != (640, 480) {
                        Self::scale_rgb_data(&rgb_data, frame_width, frame_height, 640, 480)
                    } else {
                        rgb_data
                    }
                } else {
                    // Create a black frame as fallback
                    vec![0u8; 640 * 480 * 3]
                };
                
                // Write frame to FFmpeg stdin
                if let Err(e) = stdin.write_all(&final_rgb_data).await {
                    eprintln!("Failed to write frame to FFmpeg: {}", e);
                    break;
                }
                
                // Flush periodically (not every frame for better performance)
                if frame_count % 5 == 0 {
                    if let Err(e) = stdin.flush().await {
                        eprintln!("Failed to flush frame data to FFmpeg: {}", e);
                        break;
                    }
                }
                
                frame_count += 1;
                if frame_count % 30 == 0 {
                    println!("Sent {} frames to virtual camera via FFmpeg", frame_count);
                }
                
                // Update status
                {
                    let mut status_lock = status.lock().await;
                    status_lock.frame_count = frame_count;
                }
            }
            
            // Close stdin to signal end of stream
            drop(stdin);
            
            // Wait for FFmpeg process to finish
            match ffmpeg_process.wait().await {
                Ok(exit_status) => {
                    println!("FFmpeg process finished with status: {}", exit_status);
                }
                Err(e) => {
                    eprintln!("Error waiting for FFmpeg process: {}", e);
                }
            }
            
            println!("Linux virtual camera stopped after {} frames", frame_count);
        });
        
        Ok(())
    }

    #[cfg(target_os = "macos")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // macOS AVFoundation implementation
        let _config = self.config.clone();
        let _status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting macOS AVFoundation virtual camera...");
            
            // TODO: Implement macOS AVFoundation virtual camera
            // This would involve:
            // 1. Creating an AVCaptureDevice
            // 2. Setting up AVCaptureSession
            // 3. Streaming frames through AVSampleBufferDisplayLayer
            
            while let Some(frame_data) = frame_receiver.recv().await {
                // Process frame for AVFoundation
                println!("Processing frame for macOS virtual camera: {} bytes", frame_data.len());
                
                // Simulate frame processing
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await; // ~30 FPS
            }
            
            println!("macOS virtual camera stopped");
        });
        
        Ok(())
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Fallback implementation for unsupported platforms
        tokio::spawn(async move {
            println!("Virtual camera not supported on this platform");
            while let Some(frame_data) = frame_receiver.recv().await {
                println!("Would process frame: {} bytes", frame_data.len());
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await;
            }
        });
        Ok(())
    }

    async fn stop_platform_camera(&mut self) -> Result<()> {
        // Platform-specific cleanup
        #[cfg(windows)]
        if let Some(mut vcam) = self.windows_vcam.take() {
            vcam.stop()?;
        }
        
        println!("Stopping platform-specific virtual camera...");
        Ok(())
    }

    #[cfg(windows)]
    fn jpeg_to_rgb(jpeg_data: &[u8]) -> Result<(Vec<u8>, u32, u32)> {
        // Decode JPEG to image
        let img = image::load_from_memory(jpeg_data)?;
        let rgb_img = img.to_rgb8();
        
        let width = rgb_img.width();
        let height = rgb_img.height();
        
        // Convert to raw RGB bytes
        Ok((rgb_img.into_raw(), width, height))
    }

    #[cfg(target_os = "linux")]
    fn find_loopback_device() -> Result<(String, usize)> {
        // Look for v4l2loopback devices
        for i in 0..20 {
            let device_path = format!("/dev/video{}", i);
            if std::path::Path::new(&device_path).exists() {
                // Try to open the device and check if it's a v4l2loopback device
                match Device::new(i) {
                    Ok(device) => {
                        // Check if it's a v4l2loopback device by checking capabilities and driver info
                        if let Ok(caps) = device.query_caps() {
                            println!("Checking device {}: driver='{}', card='{}', bus='{}'", 
                                device_path, caps.driver, caps.card, caps.bus);
                            
                            // Check if it's a v4l2loopback device
                            if Self::is_v4l2loopback_device(&caps) {
                                println!("Found v4l2loopback device: {}", device_path);
                                return Ok((device_path, i));
                            } else {
                                println!("Device {} is not a v4l2loopback device", device_path);
                            }
                        }
                    }
                    Err(e) => {
                        println!("Failed to open device {}: {}", device_path, e);
                        continue;
                    }
                }
            }
        }
        Err(anyhow::anyhow!("No v4l2loopback device found"))
    }

    #[cfg(target_os = "linux")]
    fn is_v4l2loopback_device(caps: &capability::Capabilities) -> bool {
        // Check if the driver is v4l2loopback
        let driver_name = caps.driver.to_lowercase();
        if driver_name.contains("v4l2loopback") || driver_name.contains("v4l2 loopback") {
            return true;
        }
        
        // Check if the card name indicates it's a loopback device
        let card_name = caps.card.to_lowercase();
        if card_name.contains("loopback") || card_name.contains("dummy") {
            return true;
        }
        
        // Check capabilities - v4l2loopback devices typically support both capture and output
        let has_video_capture = caps.capabilities.contains(capability::Flags::VIDEO_CAPTURE);
        let has_video_output = caps.capabilities.contains(capability::Flags::VIDEO_OUTPUT);
        let has_streaming = caps.capabilities.contains(capability::Flags::STREAMING);
        
        // v4l2loopback devices usually have both capture and output capabilities
        if has_video_capture && has_video_output && has_streaming {
            println!("Device has loopback-like capabilities (capture + output + streaming)");
            return true;
        }
        
        // Check if it's a writable device (can be used as output)
        if has_video_output && has_streaming {
            println!("Device has output capabilities, might be usable as virtual camera");
            return true;
        }
        
        false
    }

    #[cfg(target_os = "linux")]
    fn jpeg_to_raw_rgb(jpeg_data: &[u8]) -> Result<(Vec<u8>, usize, usize)> {
        // Decode JPEG to image
        let img = image::load_from_memory(jpeg_data)?;
        let rgb_img = img.to_rgb8();
        
        let width = rgb_img.width() as usize;
        let height = rgb_img.height() as usize;
        
        // Convert to raw RGB bytes
        Ok((rgb_img.into_raw(), width, height))
    }

    #[cfg(target_os = "linux")]
    fn scale_rgb_data(rgb_data: &[u8], src_width: usize, src_height: usize, dst_width: usize, dst_height: usize) -> Vec<u8> {
        let mut scaled_data = vec![0u8; dst_width * dst_height * 3];
        
        // Simple nearest neighbor scaling
        for dst_y in 0..dst_height {
            for dst_x in 0..dst_width {
                let src_x = (dst_x * src_width) / dst_width;
                let src_y = (dst_y * src_height) / dst_height;
                
                let src_idx = (src_y * src_width + src_x) * 3;
                let dst_idx = (dst_y * dst_width + dst_x) * 3;
                
                if src_idx + 2 < rgb_data.len() && dst_idx + 2 < scaled_data.len() {
                    scaled_data[dst_idx] = rgb_data[src_idx];     // R
                    scaled_data[dst_idx + 1] = rgb_data[src_idx + 1]; // G
                    scaled_data[dst_idx + 2] = rgb_data[src_idx + 2]; // B
                }
            }
        }
        
        scaled_data
    }
}

// Global virtual camera instance
static VIRTUAL_CAMERA: Lazy<Arc<Mutex<Option<VirtualCamera>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(None))
});

// Public API functions
pub async fn start_virtual_camera(config: Option<VirtualCameraConfig>) -> Result<VirtualCameraStatus> {
    let config = config.unwrap_or_default();
    let mut camera = VirtualCamera::new(config);
    
    camera.start().await?;
    let status = camera.get_status().await;
    
    // Store the camera instance
    let mut global_camera = VIRTUAL_CAMERA.lock().await;
    *global_camera = Some(camera);
    
    Ok(status)
}

pub async fn stop_virtual_camera() -> Result<VirtualCameraStatus> {
    let mut global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref mut camera) = global_camera.as_mut() {
        camera.stop().await?;
        let status = camera.get_status().await;
        *global_camera = None;
        Ok(status)
    } else {
        Err(anyhow::anyhow!("Virtual camera not running"))
    }
}

pub async fn get_virtual_camera_status() -> VirtualCameraStatus {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref camera) = global_camera.as_ref() {
        camera.get_status().await
    } else {
        VirtualCameraStatus {
            is_active: false,
            camera_name: "CamLooper Virtual Camera".to_string(),
            resolution: "1920x1080".to_string(),
            fps: 30,
            frame_count: 0,
        }
    }
}

pub async fn send_frame_to_virtual_camera(frame_data: Vec<u8>) -> Result<()> {
    let global_camera = VIRTUAL_CAMERA.lock().await;
    if let Some(ref camera) = global_camera.as_ref() {
        camera.send_frame(frame_data).await
    } else {
        Err(anyhow::anyhow!("Virtual camera not running"))
    }
}

#[cfg(target_os = "windows")]
pub async fn list_video_devices() -> Result<Vec<String>> {
    windows_vcam::list_video_devices()
}

#[cfg(target_os = "linux")]
pub async fn list_video_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    
    for i in 0..20 {
        let device_path = format!("/dev/video{}", i);
        if std::path::Path::new(&device_path).exists() {
            match Device::new(i) {
                Ok(device) => {
                    if let Ok(caps) = device.query_caps() {
                        devices.push(format!("{}: {} ({})", device_path, caps.card, caps.driver));
                    } else {
                        devices.push(format!("{}: Unknown device", device_path));
                    }
                }
                Err(_) => continue,
            }
        }
    }
    
    Ok(devices)
}

#[cfg(not(any(target_os = "windows", target_os = "linux")))]
pub async fn list_video_devices() -> Result<Vec<String>> {
    Ok(vec!["Virtual camera not supported on this platform".to_string()])
} 