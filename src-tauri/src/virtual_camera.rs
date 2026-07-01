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

#[cfg(target_os = "windows")]
use crate::custom_virtual_camera::CustomVirtualCamera;

// #[cfg(windows)]
// use virtualcam_rs; // TODO: Re-enable when virtualcam-rs API is properly documented

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
        }
    }

    pub async fn start(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            // Log softcam status before starting
            match crate::softcam_manager::get_softcam_status().await {
                Ok(status) => println!("[Softcam Status Before Start]:\n{}", status),
                Err(e) => println!("[Softcam Status Before Start]: Error: {}", e),
            }
        }
        {
            let is_running = self.is_running.lock().await;
            if *is_running {
                return Ok(());
            }
        }

        // Create frame channel
        let (frame_sender, frame_receiver) = mpsc::unbounded_channel::<Vec<u8>>();
        self.frame_sender = Some(frame_sender);

        // Start platform-specific virtual camera
        self.start_platform_camera(frame_receiver).await?;

        // Update status
        {
            let mut status = self.status.lock().await;
            status.is_active = true;
            status.frame_count = 0;
        }

        {
            let mut is_running = self.is_running.lock().await;
            *is_running = true;
        }
        
        println!("Virtual camera started: {}", self.config.camera_name);
        #[cfg(target_os = "windows")]
        {
            // Log softcam status after starting
            match crate::softcam_manager::get_softcam_status().await {
                Ok(status) => println!("[Softcam Status After Start]:\n{}", status),
                Err(e) => println!("[Softcam Status After Start]: Error: {}", e),
            }
        }
        Ok(())
    }

    pub async fn stop(&mut self) -> Result<()> {
        #[cfg(target_os = "windows")]
        {
            // Log softcam status before stopping
            match crate::softcam_manager::get_softcam_status().await {
                Ok(status) => println!("[Softcam Status Before Stop]:\n{}", status),
                Err(e) => println!("[Softcam Status Before Stop]: Error: {}", e),
            }
        }
        {
            let is_running = self.is_running.lock().await;
            if !*is_running {
                return Ok(());
            }
        }

        // Stop frame sender
        self.frame_sender = None;

        // Stop platform-specific virtual camera
        self.stop_platform_camera().await?;

        // Update status
        {
            let mut status = self.status.lock().await;
            status.is_active = false;
        }

        {
            let mut is_running = self.is_running.lock().await;
            *is_running = false;
        }
        
        println!("Virtual camera stopped: {}", self.config.camera_name);
        #[cfg(target_os = "windows")]
        {
            // Log softcam status after stopping
            match crate::softcam_manager::get_softcam_status().await {
                Ok(status) => println!("[Softcam Status After Stop]:\n{}", status),
                Err(e) => println!("[Softcam Status After Stop]: Error: {}", e),
            }
        }
        Ok(())
    }

    pub async fn get_status(&self) -> VirtualCameraStatus {
        self.status.lock().await.clone()
    }

    pub async fn send_frame(&self, frame_data: Vec<u8>) -> Result<()> {
        if let Some(sender) = &self.frame_sender {
            if let Err(e) = sender.send(frame_data) {
                return Err(anyhow::anyhow!("Failed to send frame to virtual camera: channel closed. This usually means the virtual camera process has stopped. Error: {}", e));
            }
            
            // Update frame count
            let mut status = self.status.lock().await;
            status.frame_count += 1;
        } else {
            return Err(anyhow::anyhow!("Virtual camera not started - no frame sender available"));
        }
        Ok(())
    }

    pub async fn is_active(&self) -> bool {
        self.status.lock().await.is_active
    }

    /// Common frame resizing method for all platforms
    fn resize_frame(rgb_data: &[u8], src_width: usize, src_height: usize, dst_width: usize, dst_height: usize) -> Vec<u8> {
        if src_width == dst_width && src_height == dst_height {
            return rgb_data.to_vec();
        }

        let mut resized_data = vec![0u8; dst_width * dst_height * 3];
        
        // Bilinear interpolation for better quality
        for dst_y in 0..dst_height {
            for dst_x in 0..dst_width {
                // Calculate source coordinates
                let src_x_f = (dst_x as f32 * src_width as f32) / dst_width as f32;
                let src_y_f = (dst_y as f32 * src_height as f32) / dst_height as f32;
                
                // Get integer coordinates
                let src_x = src_x_f as usize;
                let src_y = src_y_f as usize;
                
                // Calculate fractional parts
                let fx = src_x_f - src_x as f32;
                let fy = src_y_f - src_y as f32;
                
                // Get the four surrounding pixels
                let x1 = src_x.min(src_width - 1);
                let y1 = src_y.min(src_height - 1);
                let x2 = (src_x + 1).min(src_width - 1);
                let y2 = (src_y + 1).min(src_height - 1);
                
                // Calculate destination index
                let dst_idx = (dst_y * dst_width + dst_x) * 3;
                
                // Perform bilinear interpolation for each color channel
                for c in 0..3 {
                    let p1 = rgb_data[(y1 * src_width + x1) * 3 + c] as f32;
                    let p2 = rgb_data[(y1 * src_width + x2) * 3 + c] as f32;
                    let p3 = rgb_data[(y2 * src_width + x1) * 3 + c] as f32;
                    let p4 = rgb_data[(y2 * src_width + x2) * 3 + c] as f32;
                    
                    // Interpolate
                    let top = p1 * (1.0 - fx) + p2 * fx;
                    let bottom = p3 * (1.0 - fx) + p4 * fx;
                    let final_value = top * (1.0 - fy) + bottom * fy;
                    
                    resized_data[dst_idx + c] = final_value.round() as u8;
                }
            }
        }
        
        resized_data
    }

    /// Convert JPEG to RGB and resize to target dimensions
    fn jpeg_to_rgb_resized(jpeg_data: &[u8], target_width: u32, target_height: u32) -> Result<Vec<u8>> {
        // Decode JPEG to image
        let img = image::load_from_memory(jpeg_data)?;
        let rgb_img = img.to_rgb8();
        
        let src_width = rgb_img.width() as usize;
        let src_height = rgb_img.height() as usize;
        let raw_data = rgb_img.into_raw();
        
        // Resize to target dimensions
        let resized_data = Self::resize_frame(&raw_data, src_width, src_height, target_width as usize, target_height as usize);
        
        Ok(resized_data)
    }

    #[cfg(target_os = "windows")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Windows implementation using the custom DirectShow virtual camera
        let config = self.config.clone();
        let status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting custom DirectShow virtual camera...");
            
            // Create and start the custom virtual camera
            let mut vcam = match CustomVirtualCamera::new(
                &config.camera_name,
                config.width,
                config.height,
                config.fps,
            ) {
                Ok(vcam) => vcam,
                Err(e) => {
                    eprintln!("Failed to create custom virtual camera: {}", e);
                    eprintln!("This will cause the virtual camera channel to close");
                    return;
                }
            };
            
            if let Err(e) = vcam.start().await {
                eprintln!("Failed to start custom virtual camera: {}", e);
                eprintln!("This will cause the virtual camera channel to close");
                return;
            }
            
            let mut frame_count = 0u64;
            let target_frame_duration = tokio::time::Duration::from_millis(1000 / config.fps as u64);
            let mut frame_timer = tokio::time::interval(target_frame_duration);
            frame_timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            
            // Buffer for latest frame
            let mut latest_frame: Option<Vec<u8>> = None;
            let mut vcam_available = true;
            
            loop {
                // Wait for the next frame time
                frame_timer.tick().await;
                
                // Collect any available frames (non-blocking)
                while let Ok(frame_data) = frame_receiver.try_recv() {
                    latest_frame = Some(frame_data);
                }
                
                // Process frame and send to virtual camera (only if available)
                if vcam_available {
                    if let Some(ref frame_data) = latest_frame {
                        // Convert JPEG to RGB and resize to configured dimensions
                        match Self::jpeg_to_rgb_resized(frame_data, config.width, config.height) {
                            Ok(rgb_data) => {
                                // Send RGB frame to virtual camera
                                if let Err(e) = vcam.send_frame(&rgb_data) {
                                    eprintln!("Failed to send frame to custom virtual camera: {}", e);
                                    // Don't close the channel, just mark virtual camera as unavailable
                                    vcam_available = false;
                                }
                            }
                            Err(e) => {
                                eprintln!("Failed to convert JPEG to RGB: {}", e);
                            }
                        }
                    } else {
                        // Send black frame if no data available
                        let black_frame = vec![0u8; (config.width * config.height * 3) as usize];
                        if let Err(e) = vcam.send_frame(&black_frame) {
                            eprintln!("Failed to send black frame to custom virtual camera: {}", e);
                            // Don't close the channel, just mark virtual camera as unavailable
                            vcam_available = false;
                        }
                    }
                } else {
                    // Virtual camera is not available, but keep the channel open
                    if frame_count % 30 == 0 {
                        println!("Custom virtual camera unavailable, but keeping channel open. Frame count: {}", frame_count);
                    }
                }
                
                frame_count += 1;
                if frame_count % 30 == 0 && vcam_available {
                    println!("Sent {} frames to custom virtual camera", frame_count);
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
        let config = self.config.clone();
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
                    "-analyzeduration", "0",
                    "-probesize", "32768",
                    "-f", "mjpeg",
                    "-i", "pipe:0",
                    "-f", "v4l2",
                    "-pix_fmt", "rgb24", // Must match v4l2loopback's current device lock!

                    "-vf", &format!("scale={}:{},fps={},vflip", config.width, config.height, config.fps),
                    &device_path,
                ])
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .kill_on_drop(true)
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
            let mut last_frame_jpeg: Option<Vec<u8>> = None;
            let target_frame_duration = tokio::time::Duration::from_millis(1000 / config.fps as u64);
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
            
            println!("Virtual camera frame processing started with resolution {}x{} at {} FPS", 
                config.width, config.height, config.fps);
            
            loop {
                // Wait for the next frame time
                frame_timer.tick().await;
                
                // Collect any available frames (non-blocking)
                let mut is_disconnected = false;
                loop {
                    match buffer_receiver.try_recv() {
                        Ok(frame_data) => {
                            if frame_buffer.len() >= MAX_BUFFER_SIZE {
                                frame_buffer.pop_front();
                            }
                            frame_buffer.push_back(frame_data);
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                            break;
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {
                            is_disconnected = true;
                            break;
                        }
                    }
                }
                
                if is_disconnected {
                    println!("Virtual camera channel disconnected, stopping FFmpeg writer");
                    break;
                }
                
                // Get the most recent frame to send
                let current_frame_data = if let Some(frame_data) = frame_buffer.pop_back() {
                    // Clear any remaining buffered frames to use the latest
                    frame_buffer.clear();
                    Some(frame_data)
                } else {
                    None
                };
                
                // Use the new frame or repeat the last frame
                let jpeg_to_send = if let Some(frame_data) = current_frame_data {
                    last_frame_jpeg = Some(frame_data.clone());
                    Some(frame_data)
                } else {
                    last_frame_jpeg.clone()
                };
                
                // Send frame to FFmpeg
                if let Some(jpeg_data) = jpeg_to_send {
                    if let Err(e) = stdin.write_all(&jpeg_data).await {
                        eprintln!("Failed to write frame to FFmpeg: {}", e);
                        break;
                    }
                } else {
                    // No frames have been received yet, just wait.
                    continue;
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

    async fn stop_platform_camera(&self) -> Result<()> {
        // Platform-specific cleanup - simplified for now
        println!("Stopping platform-specific virtual camera...");
        Ok(())
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
    // Use the custom virtual camera implementation
    crate::custom_virtual_camera::list_video_devices()
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