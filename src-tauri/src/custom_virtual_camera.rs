use anyhow::{anyhow, Result};
use std::os::raw::c_void;
use std::sync::Arc;
use tokio::sync::Mutex;
use once_cell::sync::Lazy;

// FFI bindings for the custom filter
#[link(name = "CustomVideoSource")]
extern "C" {
    fn CreateCustomVideoSource() -> *mut c_void;
    fn DestroyCustomVideoSource(source: *mut c_void);
    fn SetFrameData(source: *mut c_void, data: *const u8, size: u32) -> i32;
    fn SetResolution(source: *mut c_void, width: u32, height: u32) -> i32;
    fn SetFramerate(source: *mut c_void, fps: u32) -> i32;
    fn GetStatus(source: *mut c_void) -> *const i8;
}

/// Custom virtual camera implementation using simplified FFI
pub struct CustomVirtualCamera {
    camera_name: String,
    width: u32,
    height: u32,
    fps: u32,
    frame_size: usize,
    custom_source: Option<*mut c_void>,
    is_running: bool,
}

// Make the struct Send-safe for cross-compilation
unsafe impl Send for CustomVirtualCamera {}

impl CustomVirtualCamera {
    pub fn new(camera_name: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        let frame_size = (width * height * 3) as usize;
        
        Ok(Self {
            camera_name: camera_name.to_string(),
            width,
            height,
            fps,
            frame_size,
            custom_source: None,
            is_running: false,
        })
    }

    /// Start the custom virtual camera
    pub async fn start(&mut self) -> Result<()> {
        if self.is_running {
            println!("Custom virtual camera already running");
            return Ok(());
        }

        println!("Starting custom virtual camera: {}x{} @ {}fps", 
                self.width, self.height, self.fps);

        // Create custom source filter
        unsafe {
            let source = CreateCustomVideoSource();
            if source.is_null() {
                return Err(anyhow!("Failed to create custom video source"));
            }
            
            // Configure the source
            if SetResolution(source, self.width, self.height) != 0 {
                DestroyCustomVideoSource(source);
                return Err(anyhow!("Failed to set resolution"));
            }
            
            if SetFramerate(source, self.fps) != 0 {
                DestroyCustomVideoSource(source);
                return Err(anyhow!("Failed to set framerate"));
            }
            
            self.custom_source = Some(source);
        }
        
        self.is_running = true;
        println!("Custom virtual camera started successfully");
        Ok(())
    }

    /// Stop the custom virtual camera
    pub fn stop(&mut self) -> Result<()> {
        if !self.is_running {
            return Ok(());
        }

        println!("Stopping custom virtual camera...");

        // Clean up custom source
        if let Some(source) = self.custom_source {
            unsafe {
                DestroyCustomVideoSource(source);
            }
        }
        
        self.custom_source = None;
        self.is_running = false;

        println!("Custom virtual camera stopped");
        Ok(())
    }

    /// Send a frame to the virtual camera
    pub fn send_frame(&mut self, frame_data: &[u8]) -> Result<()> {
        if !self.is_running {
            return Err(anyhow!("Custom virtual camera not running"));
        }

        if frame_data.len() != self.frame_size {
            return Err(anyhow!(
                "Frame size mismatch. Expected {}, got {}",
                self.frame_size,
                frame_data.len()
            ));
        }

        if let Some(source) = self.custom_source {
            unsafe {
                let result = SetFrameData(source, frame_data.as_ptr(), frame_data.len() as u32);
                if result != 0 {
                    return Err(anyhow!("Failed to set frame data: {}", result));
                }
            }
        } else {
            return Err(anyhow!("Custom source filter not available"));
        }

        Ok(())
    }

    /// Get camera status
    pub fn get_status(&self) -> String {
        if let Some(source) = self.custom_source {
            unsafe {
                let status_ptr = GetStatus(source);
                if !status_ptr.is_null() {
                    let status_str = std::ffi::CStr::from_ptr(status_ptr);
                    return format!(
                        "Custom Virtual Camera Status:\n\
                        Name: {}\n\
                        {}\n\
                        Running: {}",
                        self.camera_name,
                        status_str.to_string_lossy(),
                        self.is_running
                    );
                }
            }
        }
        
        format!(
            "Custom Virtual Camera Status:\n\
            Name: {}\n\
            Resolution: {}x{}\n\
            FPS: {}\n\
            Running: {}\n\
            Frame Size: {} bytes\n\
            Status: Not initialized",
            self.camera_name,
            self.width,
            self.height,
            self.fps,
            self.is_running,
            self.frame_size
        )
    }
}

impl Drop for CustomVirtualCamera {
    fn drop(&mut self) {
        if self.is_running {
            let _ = self.stop();
        }
    }
}

// Global instance for easy access
static CUSTOM_CAMERA: Lazy<Arc<Mutex<Option<CustomVirtualCamera>>>> = Lazy::new(|| {
    Arc::new(Mutex::new(None))
});

/// Initialize the global custom camera
pub async fn init_custom_camera() -> Result<()> {
    let mut global_camera = CUSTOM_CAMERA.lock().await;
    *global_camera = None;
    Ok(())
}

/// Get the global custom camera
pub async fn get_custom_camera() -> Result<CustomVirtualCamera> {
    let global_camera = CUSTOM_CAMERA.lock().await;
    if let Some(ref camera) = *global_camera {
        // Create a new instance with the same configuration
        CustomVirtualCamera::new(&camera.camera_name, camera.width, camera.height, camera.fps)
    } else {
        Err(anyhow!("Custom camera not initialized"))
    }
}

/// Start custom virtual camera
pub async fn start_custom_camera(camera_name: &str, width: u32, height: u32, fps: u32) -> Result<()> {
    let mut camera = CustomVirtualCamera::new(camera_name, width, height, fps)?;
    camera.start().await?;
    
    let mut global_camera = CUSTOM_CAMERA.lock().await;
    *global_camera = Some(camera);
    Ok(())
}

/// Stop custom virtual camera
pub async fn stop_custom_camera() -> Result<()> {
    let mut global_camera = CUSTOM_CAMERA.lock().await;
    if let Some(ref mut camera) = global_camera.as_mut() {
        camera.stop()?;
        *global_camera = None;
    }
    Ok(())
}

/// Send frame to custom virtual camera
pub async fn send_frame_to_custom_camera(frame_data: &[u8]) -> Result<()> {
    let mut global_camera = CUSTOM_CAMERA.lock().await;
    if let Some(ref mut camera) = global_camera.as_mut() {
        camera.send_frame(frame_data)
    } else {
        Err(anyhow!("Custom camera not running"))
    }
}

/// Get custom camera status
pub async fn get_custom_camera_status() -> Result<String> {
    let global_camera = CUSTOM_CAMERA.lock().await;
    if let Some(ref camera) = global_camera.as_ref() {
        Ok(camera.get_status())
    } else {
        Ok("Custom camera not initialized".to_string())
    }
}

/// List available video devices (including custom camera)
pub fn list_video_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    
    // Add our custom camera
    devices.push("Custom CamLooper Virtual Camera".to_string());
    
    // In a real implementation, this would enumerate all DirectShow devices
    // including our custom camera and any other available cameras
    
    Ok(devices)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_custom_camera_creation() {
        let camera = CustomVirtualCamera::new("Test Camera", 1920, 1080, 30);
        assert!(camera.is_ok());
        
        let camera = camera.unwrap();
        assert_eq!(camera.width, 1920);
        assert_eq!(camera.height, 1080);
        assert_eq!(camera.fps, 30);
        assert_eq!(camera.frame_size, 1920 * 1080 * 3);
    }

    #[test]
    fn test_device_listing() {
        let devices = list_video_devices();
        assert!(devices.is_ok());
        
        let devices = devices.unwrap();
        assert!(!devices.is_empty());
        assert!(devices.contains(&"Custom CamLooper Virtual Camera".to_string()));
    }
} 