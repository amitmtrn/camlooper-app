//! Windows Virtual Camera Implementation using DirectShow and softcam
//! 
//! This module provides a Windows-specific virtual camera implementation that creates
//! actual virtual camera devices that Windows applications can detect and use.
//! 
//! The implementation uses the softcam library to create DirectShow virtual cameras.

use anyhow::Result;
use std::ffi::CString;
use std::os::raw::{c_float, c_int, c_void};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryA};
use crate::softcam_manager;

/// Handle to a softcam virtual camera instance
pub type ScCamera = *mut c_void;

/// Function pointer types for softcam's sender API (see tshino/softcam softcam.h).
/// Note the framerate is a C `float`, and `scSendFrame` returns `void`.
type ScCreateCameraFn = unsafe extern "C" fn(width: c_int, height: c_int, framerate: c_float) -> ScCamera;
type ScSendFrameFn = unsafe extern "C" fn(cam: ScCamera, frame_data: *const u8);
type ScDeleteCameraFn = unsafe extern "C" fn(cam: ScCamera);

/// Softcam function pointers loaded from DLL
struct SoftcamFunctions {
    create_camera: ScCreateCameraFn,
    send_frame: ScSendFrameFn,
    delete_camera: ScDeleteCameraFn,
}

/// Windows Virtual Camera using softcam library
pub struct WindowsVirtualCamera {
    camera_handle: Option<ScCamera>,
    width: u32,
    height: u32,
    fps: u32,
    camera_name: String,
    frame_size: usize,
    dll_handle: Option<windows::Win32::Foundation::HINSTANCE>,
    functions: Option<SoftcamFunctions>,
}

// SAFETY: This struct contains raw pointers but they are only used in the context
// of the Windows API calls and are properly managed. The camera handle is created
// and destroyed within the same thread context.
unsafe impl Send for WindowsVirtualCamera {}

impl WindowsVirtualCamera {
    /// Create a new Windows virtual camera
    pub fn new(camera_name: &str, width: u32, height: u32, fps: u32) -> Result<Self> {
        let camera = Self {
            camera_handle: None,
            width,
            height,
            fps,
            camera_name: camera_name.to_string(),
            frame_size: (width * height * 3) as usize, // RGB24 format
            dll_handle: None,
            functions: None,
        };
        
        Ok(camera)
    }

    /// Start the virtual camera
    pub async fn start(&mut self) -> Result<()> {
        if self.camera_handle.is_some() {
            println!("Windows virtual camera already started");
            return Ok(());
        }

        println!("Starting Windows virtual camera with resolution {}x{} at {} FPS", 
                self.width, self.height, self.fps);

        // Load softcam DLL and functions
        println!("Loading softcam DLL...");
        self.load_softcam_dll().await?;
        println!("Softcam DLL loaded successfully");
        
        // Create virtual camera instance
        println!("Creating softcam virtual camera...");
        let camera_handle = self.create_softcam_camera()?;
        self.camera_handle = Some(camera_handle);
        
        println!("Windows virtual camera started successfully using softcam: {}", self.camera_name);
        Ok(())
    }

    /// Stop the virtual camera
    pub fn stop(&mut self) -> Result<()> {
        if let Some(camera_handle) = self.camera_handle.take() {
            self.destroy_softcam_camera(camera_handle)?;
            self.unload_softcam_dll()?;
            println!("Windows virtual camera stopped: {}", self.camera_name);
        }
        Ok(())
    }

    /// Send a frame to the virtual camera
    pub fn send_frame(&mut self, frame_data: &[u8]) -> Result<()> {
        if let Some(camera_handle) = self.camera_handle {
            if frame_data.len() != self.frame_size {
                return Err(anyhow::anyhow!(
                    "Frame data size mismatch. Expected {}, got {}",
                    self.frame_size,
                    frame_data.len()
                ));
            }

            if let Some(ref functions) = self.functions {
                // scSendFrame returns void; softcam handles the case where no consumer
                // is connected yet by simply buffering the latest frame.
                unsafe {
                    (functions.send_frame)(camera_handle, frame_data.as_ptr());
                }
            } else {
                return Err(anyhow::anyhow!("Softcam functions not loaded"));
            }
        }
        Ok(())
    }

    /// Load the softcam DLL and function pointers
    async fn load_softcam_dll(&mut self) -> Result<()> {
        println!("Checking softcam availability...");

        // Prefer the softcam.dll bundled next to our executable — it is installed and
        // registered by the app installer, so no download is needed.
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let bundled = dir.join("softcam.dll");
                if bundled.exists() {
                    match self.try_load_dll_from_path(&bundled) {
                        Ok(()) => {
                            println!("Loaded bundled softcam DLL from: {:?}", bundled);
                            return Ok(());
                        }
                        Err(e) => println!("Failed to load bundled softcam DLL: {}", e),
                    }
                }
            }
        }

        // Fallback: try the softcam manager (legacy download path)
        match softcam_manager::get_softcam_dll_path().await {
            Ok(dll_path) => {
                println!("Found softcam DLL at: {:?}", dll_path);
                if let Err(e) = self.try_load_dll_from_path(&dll_path) {
                    println!("Failed to load DLL from manager path: {}", e);
                    println!("Attempting automatic softcam setup...");
                    // Fall back to trying to setup softcam automatically
                    self.try_auto_setup_softcam().await?;
                } else {
                    println!("Successfully loaded softcam DLL from manager path");
                }
            }
            Err(e) => {
                println!("Softcam DLL not found in manager: {}", e);
                println!("Attempting automatic softcam setup...");
                // Softcam not available, try to set it up automatically
                self.try_auto_setup_softcam().await?;
            }
        }
        
        Ok(())
    }
    
    async fn try_auto_setup_softcam(&mut self) -> Result<()> {
        println!("Attempting to setup softcam automatically...");
        
        // Check if already available
        if !softcam_manager::is_softcam_available().await {
            println!("Softcam not available, downloading and setting up...");
            
            // Try to setup softcam
            match softcam_manager::setup_softcam().await {
                Ok(_) => {
                    println!("Softcam setup completed successfully");
                }
                Err(e) => {
                    return Err(anyhow::anyhow!(
                        "Failed to setup softcam automatically: {}\n\
                        \n\
                        Please install softcam manually:\n\
                        1. Download from: https://github.com/tshino/softcam/releases\n\
                        2. Extract the files\n\
                        3. Run as Administrator: regsvr32 softcam.dll\n\
                        4. Restart your application",
                        e
                    ));
                }
            }
        }
        
        // Now try to load the DLL
        match softcam_manager::get_softcam_dll_path().await {
            Ok(dll_path) => {
                self.try_load_dll_from_path(&dll_path)
            }
            Err(e) => {
                Err(anyhow::anyhow!("Failed to get softcam DLL path after setup: {}", e))
            }
        }
    }
    
    fn try_load_dll_from_path(&mut self, dll_path: &std::path::Path) -> Result<()> {
        let path_str = dll_path.to_str()
            .ok_or_else(|| anyhow::anyhow!("Invalid DLL path: {:?}", dll_path))?;
        let path_cstr = CString::new(path_str)?;
        
        unsafe {
            let handle = LoadLibraryA(windows::core::PCSTR(path_cstr.as_ptr() as *const u8))?;
            if !handle.is_invalid() {
                self.dll_handle = Some(handle.into());
                println!("Loaded softcam DLL from: {}", path_str);
                
                // Load function pointers
                self.load_softcam_functions(handle.into())?;
                return Ok(());
            }
        }
        
        Err(anyhow::anyhow!("Failed to load softcam DLL from: {}", path_str))
    }

    /// Load softcam function pointers from the DLL
    fn load_softcam_functions(&mut self, dll_handle: windows::Win32::Foundation::HINSTANCE) -> Result<()> {
        unsafe {
            // Load scCreateCamera function
            let create_camera_name = CString::new("scCreateCamera")?;
            let create_camera_ptr = GetProcAddress(dll_handle, windows::core::PCSTR(create_camera_name.as_ptr() as *const u8));
            if create_camera_ptr.is_none() {
                return Err(anyhow::anyhow!("Failed to load scCreateCamera function from softcam.dll"));
            }
            
            // Load scSendFrame function
            let send_frame_name = CString::new("scSendFrame")?;
            let send_frame_ptr = GetProcAddress(dll_handle, windows::core::PCSTR(send_frame_name.as_ptr() as *const u8));
            if send_frame_ptr.is_none() {
                return Err(anyhow::anyhow!("Failed to load scSendFrame function from softcam.dll"));
            }
            
            // Load scDeleteCamera function
            let delete_camera_name = CString::new("scDeleteCamera")?;
            let delete_camera_ptr = GetProcAddress(dll_handle, windows::core::PCSTR(delete_camera_name.as_ptr() as *const u8));
            if delete_camera_ptr.is_none() {
                return Err(anyhow::anyhow!("Failed to load scDeleteCamera function from softcam.dll"));
            }

            // Create function pointers
            let functions = SoftcamFunctions {
                create_camera: std::mem::transmute(create_camera_ptr.unwrap()),
                send_frame: std::mem::transmute(send_frame_ptr.unwrap()),
                delete_camera: std::mem::transmute(delete_camera_ptr.unwrap()),
            };
            
            self.functions = Some(functions);
            println!("Loaded all softcam function pointers successfully");
            Ok(())
        }
    }

    /// Unload the softcam DLL
    fn unload_softcam_dll(&mut self) -> Result<()> {
        self.functions = None;
        if let Some(_handle) = self.dll_handle.take() {
            // Note: FreeLibrary is not available in the current windows crate version
        }
        Ok(())
    }

    /// Create a softcam virtual camera using the loaded DLL
    fn create_softcam_camera(&self) -> Result<ScCamera> {
        if let Some(ref functions) = self.functions {
            let camera_handle = unsafe {
                (functions.create_camera)(
                    self.width as c_int,
                    self.height as c_int,
                    self.fps as c_float,
                )
            };
            
            if camera_handle.is_null() {
                return Err(anyhow::anyhow!(
                    "Failed to create softcam virtual camera. Ensure:\n\
                    1. softcam.dll is properly registered (regsvr32 softcam.dll)\n\
                    2. You have administrator privileges\n\
                    3. DirectShow components are available\n\
                    4. No other application is using the virtual camera"
                ));
            }

            println!("Created softcam virtual camera: {}x{} @ {}fps", self.width, self.height, self.fps);
            Ok(camera_handle)
        } else {
            Err(anyhow::anyhow!("Softcam functions not loaded"))
        }
    }

    /// Destroy the softcam virtual camera
    fn destroy_softcam_camera(&self, camera_handle: ScCamera) -> Result<()> {
        if let Some(ref functions) = self.functions {
            unsafe {
                (functions.delete_camera)(camera_handle);
            }
            println!("Destroyed softcam virtual camera");
        }
        Ok(())
    }
}

impl Drop for WindowsVirtualCamera {
    fn drop(&mut self) {
        if let Err(e) = self.stop() {
            eprintln!("Error stopping Windows virtual camera: {}", e);
        }
    }
}

/// List available video devices on Windows via DirectShow ICreateDevEnum.
/// Enumerates CLSID_VideoInputDeviceCategory (physical UVC webcams, virtual
/// cameras registered as DirectShow filters like softcam, etc.) and reads
/// FriendlyName from each moniker's IPropertyBag.
pub fn list_video_devices() -> Result<Vec<String>> {
    use windows::Win32::Media::DirectShow::ICreateDevEnum;
    use windows::Win32::Media::MediaFoundation::{
        CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory,
    };
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, IBindCtx, IEnumMoniker, IErrorLog, IMoniker,
        StructuredStorage::IPropertyBag, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED,
    };
    use windows::core::{w, BSTR, VARIANT};

    let mut devices = Vec::new();

    unsafe {
        // STA is fine; returns S_FALSE if this thread already initialized — ignore.
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);

        let dev_enum: ICreateDevEnum =
            CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER)
                .map_err(|e| anyhow::anyhow!("CoCreateInstance(SystemDeviceEnum) failed: {e}"))?;

        // Out-param style: null enum on S_FALSE (no devices in category).
        let mut enum_moniker: Option<IEnumMoniker> = None;
        let _ = dev_enum.CreateClassEnumerator(
            &CLSID_VideoInputDeviceCategory,
            &mut enum_moniker,
            0,
        );
        let Some(enum_moniker) = enum_moniker else {
            return Ok(devices);
        };

        let mut slot: [Option<IMoniker>; 1] = [None];
        loop {
            let mut fetched: u32 = 0;
            let hr = enum_moniker.Next(&mut slot, Some(&mut fetched));
            if hr.is_err() || fetched == 0 {
                break;
            }
            let Some(moniker) = slot[0].take() else { break };

            let bag: IPropertyBag =
                match moniker.BindToStorage::<Option<&IBindCtx>, Option<&IMoniker>, IPropertyBag>(
                    None, None,
                ) {
                    Ok(b) => b,
                    Err(_) => continue,
                };

            let mut variant = VARIANT::default();
            if bag
                .Read(w!("FriendlyName"), &mut variant, None::<&IErrorLog>)
                .is_ok()
            {
                if let Ok(bstr) = BSTR::try_from(&variant) {
                    let name = bstr.to_string();
                    if !name.is_empty() {
                        devices.push(name);
                    }
                }
            }
        }
    }

    Ok(devices)
}

/// Install the softcam virtual camera driver/filter
pub fn install_softcam_driver() -> Result<()> {
    println!("To install softcam virtual camera driver:");
    println!("1. Download softcam from: https://github.com/tshino/softcam/releases");
    println!("2. Extract the downloaded files");
    println!("3. Run as Administrator: regsvr32 softcam.dll");
    println!("4. Restart your application");
    println!("5. Check if virtual camera appears in camera applications");
    
    Ok(())
}

/// Uninstall the softcam virtual camera driver/filter
pub fn uninstall_softcam_driver() -> Result<()> {
    println!("To uninstall softcam virtual camera driver:");
    println!("1. Run as Administrator: regsvr32 /u softcam.dll");
    println!("2. Remove softcam files");
    
    Ok(())
}

/// Check if softcam is available on the system
pub fn is_softcam_available() -> bool {
    // Try to get the softcam manager to check if DLL is available
    let rt = tokio::runtime::Runtime::new();
    match rt {
        Ok(runtime) => {
            runtime.block_on(async {
                softcam_manager::is_softcam_available().await
            })
        }
        Err(_) => {
            // Fallback to manual check if runtime creation fails
            is_softcam_available_sync()
        }
    }
}

/// Synchronous version of softcam availability check (fallback)
fn is_softcam_available_sync() -> bool {
    // Try to load the DLL to check availability
    let dll_paths = [
        "softcam.dll",
        "./softcam.dll",
        "C:\\Program Files\\softcam\\softcam.dll",
        "C:\\Program Files (x86)\\softcam\\softcam.dll",
    ];

    for path in &dll_paths {
        if let Ok(path_cstr) = CString::new(*path) {
            unsafe {
                if let Ok(handle) = LoadLibraryA(windows::core::PCSTR(path_cstr.as_ptr() as *const u8)) {
                    if !handle.is_invalid() {
                        // Note: FreeLibrary is not available in the current windows crate version
                        // We'll just return true if we can load the library
                        return true;
                    }
                }
            }
        }
    }

    false
}

/// Get information about the current softcam installation
pub fn get_softcam_info() -> Result<String> {
    if is_softcam_available() {
        Ok("Softcam is available and ready to use".to_string())
    } else {
        Ok("Softcam is not installed or not registered. Run install_softcam_driver() for instructions.".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_camera_creation() {
        let camera = WindowsVirtualCamera::new("Test Camera", 640, 480, 30);
        assert!(camera.is_ok());
        
        let camera = camera.unwrap();
        assert_eq!(camera.width, 640);
        assert_eq!(camera.height, 480);
        assert_eq!(camera.fps, 30);
        assert_eq!(camera.camera_name, "Test Camera");
    }

    #[test]
    fn test_device_listing() {
        let devices = list_video_devices();
        assert!(devices.is_ok());
        
        let devices = devices.unwrap();
        assert!(!devices.is_empty());
    }

    #[test]
    fn test_softcam_availability() {
        // This test will pass/fail depending on whether softcam is installed
        let available = is_softcam_available();
        let info = get_softcam_info().unwrap();
        println!("Softcam available: {}", available);
        println!("Softcam info: {}", info);
    }
} 