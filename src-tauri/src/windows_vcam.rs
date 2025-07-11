use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

// Simplified Windows implementation without COM interfaces
// This provides the basic API for Windows builds without complex DirectShow implementation

// Global state for managing virtual cameras
static VIRTUAL_CAMERA_MANAGER: Mutex<Option<VirtualCameraManager>> = Mutex::new(None);

#[derive(Debug)]
pub struct VirtualCameraManager {
    cameras: HashMap<String, VirtualCameraInstance>,
}

#[derive(Debug)]
struct VirtualCameraInstance {
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    is_active: bool,
    frame_buffer: Arc<Mutex<Option<Vec<u8>>>>,
}

pub struct WindowsVirtualCamera {
    name: String,
    width: u32,
    height: u32,
    fps: u32,
    instance_id: String,
    is_started: bool,
}

impl WindowsVirtualCamera {
    pub fn new(name: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        let instance_id = format!("{}_{}_{}_{}", name, width, height, fps);
        
        Ok(Self {
            name: name.to_string(),
            width,
            height,
            fps,
            instance_id,
            is_started: false,
        })
    }

    pub fn start(&mut self) -> Result<()> {
        if self.is_started {
            return Ok(());
        }

        // Create virtual camera instance
        let instance = VirtualCameraInstance {
            name: self.name.clone(),
            width: self.width,
            height: self.height,
            fps: self.fps,
            is_active: true,
            frame_buffer: Arc::new(Mutex::new(None)),
        };

        // Register virtual camera
        {
            let mut manager = VIRTUAL_CAMERA_MANAGER.lock().unwrap();
            if manager.is_none() {
                *manager = Some(VirtualCameraManager {
                    cameras: HashMap::new(),
                });
            }
            
            if let Some(ref mut mgr) = manager.as_mut() {
                mgr.cameras.insert(self.instance_id.clone(), instance);
            }
        }

        self.is_started = true;
        println!("Windows virtual camera started (stub): {}", self.name);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<()> {
        if !self.is_started {
            return Ok(());
        }

        // Unregister virtual camera
        {
            let mut manager = VIRTUAL_CAMERA_MANAGER.lock().unwrap();
            if let Some(ref mut mgr) = manager.as_mut() {
                mgr.cameras.remove(&self.instance_id);
            }
        }

        self.is_started = false;
        println!("Windows virtual camera stopped: {}", self.name);
        Ok(())
    }
}

impl Drop for WindowsVirtualCamera {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

// Public API functions
pub fn send_frame_to_virtual_camera(rgb_data: &[u8], width: u32, height: u32) -> Result<()> {
    let manager = VIRTUAL_CAMERA_MANAGER.lock().unwrap();
    
    if let Some(ref mgr) = manager.as_ref() {
        // Find active camera and update its frame buffer
        for (_id, camera) in &mgr.cameras {
            if camera.is_active && camera.width == width && camera.height == height {
                let mut frame_buffer = camera.frame_buffer.lock().unwrap();
                *frame_buffer = Some(rgb_data.to_vec());
                return Ok(());
            }
        }
    }
    
    // If no exact match, just succeed (frame will be dropped)
    println!("Windows virtual camera: frame sent (stub implementation)");
    Ok(())
}

pub fn list_video_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    
    // Add some default Windows devices
    devices.push("Built-in Camera".to_string());
    devices.push("USB Camera".to_string());
    
    // Add our virtual cameras
    let manager = VIRTUAL_CAMERA_MANAGER.lock().unwrap();
    if let Some(ref mgr) = manager.as_ref() {
        for (_id, camera) in &mgr.cameras {
            if camera.is_active {
                devices.push(format!("{} (Virtual - {}x{}@{}fps)", 
                    camera.name, camera.width, camera.height, camera.fps));
            }
        }
    }
    
    Ok(devices)
}