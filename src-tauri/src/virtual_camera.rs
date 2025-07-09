use anyhow::Result;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

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
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Windows DirectShow implementation
        let _config = self.config.clone();
        let _status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting Windows DirectShow virtual camera...");
            
            // TODO: Implement Windows DirectShow virtual camera
            // This would involve:
            // 1. Creating a DirectShow filter
            // 2. Registering it as a capture device
            // 3. Streaming frames to the filter
            
            while let Some(frame_data) = frame_receiver.recv().await {
                // Process frame for DirectShow
                // Convert frame_data to DirectShow format and stream
                println!("Processing frame for Windows virtual camera: {} bytes", frame_data.len());
                
                // Simulate frame processing
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await; // ~30 FPS
            }
            
            println!("Windows virtual camera stopped");
        });
        
        Ok(())
    }

    #[cfg(target_os = "linux")]
    async fn start_platform_camera(&self, mut frame_receiver: mpsc::UnboundedReceiver<Vec<u8>>) -> Result<()> {
        // Linux v4l2loopback implementation
        let _config = self.config.clone();
        let _status = self.status.clone();
        
        tokio::spawn(async move {
            println!("Starting Linux v4l2loopback virtual camera...");
            
            // TODO: Implement v4l2loopback virtual camera
            // This would involve:
            // 1. Opening /dev/video device
            // 2. Setting up video format
            // 3. Writing frames to the device
            
            while let Some(frame_data) = frame_receiver.recv().await {
                // Process frame for v4l2loopback
                println!("Processing frame for Linux virtual camera: {} bytes", frame_data.len());
                
                // Simulate frame processing
                tokio::time::sleep(tokio::time::Duration::from_millis(33)).await; // ~30 FPS
            }
            
            println!("Linux virtual camera stopped");
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
        // Platform-specific cleanup would go here
        println!("Stopping platform-specific virtual camera...");
        Ok(())
    }
}

// Global virtual camera instance
static VIRTUAL_CAMERA: Lazy<Arc<Mutex<Option<VirtualCamera>>>> = 
    Lazy::new(|| Arc::new(Mutex::new(None)));

// Helper functions for the Tauri commands
pub async fn start_virtual_camera(config: Option<VirtualCameraConfig>) -> Result<VirtualCameraStatus> {
    let config = config.unwrap_or_default();
    
    let mut camera_opt = VIRTUAL_CAMERA.lock().await;
    
    // Create new camera if none exists
    if camera_opt.is_none() {
        *camera_opt = Some(VirtualCamera::new(config));
    }
    
    if let Some(camera) = camera_opt.as_mut() {
        camera.start().await?;
        Ok(camera.get_status().await)
    } else {
        Err(anyhow::anyhow!("Failed to create virtual camera"))
    }
}

pub async fn stop_virtual_camera() -> Result<VirtualCameraStatus> {
    let mut camera_opt = VIRTUAL_CAMERA.lock().await;
    
    if let Some(camera) = camera_opt.as_mut() {
        camera.stop().await?;
        Ok(camera.get_status().await)
    } else {
        Err(anyhow::anyhow!("No virtual camera to stop"))
    }
}

pub async fn get_virtual_camera_status() -> VirtualCameraStatus {
    let camera_opt = VIRTUAL_CAMERA.lock().await;
    
    if let Some(camera) = camera_opt.as_ref() {
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
    let camera_opt = VIRTUAL_CAMERA.lock().await;
    
    if let Some(camera) = camera_opt.as_ref() {
        if camera.is_active().await {
            camera.send_frame(frame_data).await?;
        }
    }
    
    Ok(())
} 