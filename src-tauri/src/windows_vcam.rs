use anyhow::{anyhow, Result};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use widestring::U16CString;

use windows::{
    core::*,
    Win32::{
        Foundation::*,
        Media::DirectShow::*,
        Media::MediaFoundation::*,
        System::Com::*,
        System::Registry::*,
        Graphics::Gdi::*,
        System::Memory::*,
    },
    implement,
};

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

        // Initialize COM
        unsafe {
            let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            match hr {
                S_OK | S_FALSE | RPC_E_CHANGED_MODE => {},
                _ => return Err(anyhow!("Failed to initialize COM: {:?}", hr)),
            }
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

        // Register DirectShow filter (simplified version)
        self.register_directshow_filter()?;

        self.is_started = true;
        println!("Windows virtual camera started: {}", self.name);
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

        // Unregister DirectShow filter
        self.unregister_directshow_filter()?;

        self.is_started = false;
        println!("Windows virtual camera stopped: {}", self.name);
        Ok(())
    }

    fn register_directshow_filter(&self) -> Result<()> {
        // This is a simplified registration process
        // In a real implementation, you would register a proper DirectShow filter
        println!("Registering DirectShow filter for virtual camera: {}", self.name);
        
        // For now, we'll just simulate registration
        // A real implementation would involve creating and registering a COM server
        // that implements the DirectShow filter interfaces
        
        println!("Virtual camera registration simulated (requires admin privileges for real registry operations)");
        Ok(())
    }

    fn unregister_directshow_filter(&self) -> Result<()> {
        println!("Unregistering DirectShow filter for virtual camera: {}", self.name);
        // Cleanup registry entries if needed
        Ok(())
    }
}

impl Drop for WindowsVirtualCamera {
    fn drop(&mut self) {
        let _ = self.stop();
        unsafe {
            CoUninitialize();
        }
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
    Ok(())
}

pub fn list_video_devices() -> Result<Vec<String>> {
    let mut devices = Vec::new();
    
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match hr {
            S_OK | S_FALSE | RPC_E_CHANGED_MODE => {},
            _ => return Err(anyhow!("Failed to initialize COM: {:?}", hr)),
        }
        
        // Create system device enumerator
        let device_enum_result: Result<ICreateDevEnum, _> = CoCreateInstance(
            &CLSID_SystemDeviceEnum,
            None,
            CLSCTX_INPROC_SERVER,
        );
        
        if let Ok(device_enum) = device_enum_result {
            // Enumerate video capture devices
            if let Ok(enum_moniker) = device_enum.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, 0) {
                if let Ok(enumerator) = enum_moniker {
                    let mut moniker: Option<IMoniker> = None;
                    
                    while enumerator.Next(1, &mut moniker, None) == S_OK {
                        if let Some(ref mk) = moniker {
                            // Get property bag
                            if let Ok(prop_bag_unknown) = mk.BindToStorage(None, None, &IPropertyBag::IID) {
                                if let Ok(prop_bag) = prop_bag_unknown.cast::<IPropertyBag>() {
                                    // Get friendly name
                                    let mut var = VARIANT::default();
                                    if prop_bag.Read(w!("FriendlyName"), &mut var, None).is_ok() {
                                        if let Ok(name) = var.Anonymous.Anonymous.Anonymous.bstrVal.to_string() {
                                            devices.push(name);
                                        }
                                    }
                                    let _ = VariantClear(&mut var);
                                }
                            }
                        }
                        moniker = None;
                    }
                }
            }
        }
        
        CoUninitialize();
    }
    
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

// Simplified DirectShow virtual camera filter implementation
#[implement(IUnknown, IBaseFilter)]
pub struct VirtualCameraFilter {
    ref_count: std::sync::atomic::AtomicU32,
    name: String,
    width: u32,
    height: u32,
    fps: u32,
}

impl VirtualCameraFilter {
    pub fn new(name: String, width: u32, height: u32, fps: u32) -> Self {
        Self {
            ref_count: std::sync::atomic::AtomicU32::new(1),
            name,
            width,
            height,
            fps,
        }
    }
}

impl IUnknown_Impl for VirtualCameraFilter {
    fn QueryInterface(&self, riid: *const GUID, ppvobject: *mut *mut std::ffi::c_void) -> HRESULT {
        unsafe {
            *ppvobject = std::ptr::null_mut();
            
            if IsEqualIID(riid, &IUnknown::IID) || IsEqualIID(riid, &IBaseFilter::IID) {
                *ppvobject = self as *const _ as *mut std::ffi::c_void;
                self.AddRef();
                S_OK
            } else {
                E_NOINTERFACE
            }
        }
    }
    
    fn AddRef(&self) -> u32 {
        self.ref_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1
    }
    
    fn Release(&self) -> u32 {
        let count = self.ref_count.fetch_sub(1, std::sync::atomic::Ordering::SeqCst) - 1;
        if count == 0 {
            // Would deallocate here in a real implementation
        }
        count
    }
}

impl IBaseFilter_Impl for VirtualCameraFilter {
    fn GetClassID(&self, pclsid: *mut GUID) -> HRESULT {
        unsafe {
            if pclsid.is_null() {
                return E_POINTER;
            }
            // Use a dummy CLSID for our virtual camera
            *pclsid = GUID::from_u128(0x12345678_1234_1234_1234_123456789ABC);
            S_OK
        }
    }
    
    fn Stop(&self) -> HRESULT {
        println!("Virtual camera filter stopped");
        S_OK
    }
    
    fn Pause(&self) -> HRESULT {
        println!("Virtual camera filter paused");
        S_OK
    }
    
    fn Run(&self, tstart: i64) -> HRESULT {
        println!("Virtual camera filter running from time: {}", tstart);
        S_OK
    }
    
    fn GetState(&self, _dwmillisecstimeout: u32, state: *mut FILTER_STATE) -> HRESULT {
        unsafe {
            if state.is_null() {
                return E_POINTER;
            }
            *state = State_Running;
            S_OK
        }
    }
    
    fn SetSyncSource(&self, _pclock: Option<&IReferenceClock>) -> HRESULT {
        // Accept any clock
        S_OK
    }
    
    fn GetSyncSource(&self, pclock: *mut Option<IReferenceClock>) -> HRESULT {
        unsafe {
            if pclock.is_null() {
                return E_POINTER;
            }
            *pclock = None;
            S_OK
        }
    }
    
    fn EnumPins(&self, _ppenum: *mut Option<IEnumPins>) -> HRESULT {
        // Would enumerate output pins here
        E_NOTIMPL
    }
    
    fn FindPin(&self, _id: &PCWSTR, _pppin: *mut Option<IPin>) -> HRESULT {
        // Would find pin by ID here
        E_NOTIMPL
    }
    
    fn QueryFilterInfo(&self, pinfo: *mut FILTER_INFO) -> HRESULT {
        unsafe {
            if pinfo.is_null() {
                return E_POINTER;
            }
            
            let name_wide = U16CString::from_str(&self.name).unwrap_or_default();
            let name_slice = name_wide.as_slice();
            let copy_len = std::cmp::min(name_slice.len(), 128);
            
            std::ptr::copy_nonoverlapping(
                name_slice.as_ptr(),
                (*pinfo).achName.as_mut_ptr(),
                copy_len,
            );
            
            if copy_len < 128 {
                (*pinfo).achName[copy_len] = 0;
            }
            
            (*pinfo).pGraph = None;
            S_OK
        }
    }
    
    fn JoinFilterGraph(&self, _pgraph: Option<&IFilterGraph>, _pname: &PCWSTR) -> HRESULT {
        println!("Virtual camera filter joined to graph");
        S_OK
    }
    
    fn QueryVendorInfo(&self, _pvendorinfo: *mut PWSTR) -> HRESULT {
        E_NOTIMPL
    }
}

// Utility function to ensure COM is initialized
pub fn ensure_com_initialized() -> Result<()> {
    unsafe {
        let hr = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        match hr {
            S_OK | S_FALSE | RPC_E_CHANGED_MODE => Ok(()),
            _ => Err(anyhow!("Failed to initialize COM: {:?}", hr)),
        }
    }
}

// Utility function to create a bitmap from RGB data
pub fn create_bitmap_from_rgb(rgb_data: &[u8], width: u32, height: u32) -> Result<HBITMAP> {
    unsafe {
        let hdc = GetDC(HWND::default());
        if hdc.is_invalid() {
            return Err(anyhow!("Failed to get device context"));
        }
        
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // Negative for top-down bitmap
                biPlanes: 1,
                biBitCount: 24,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD::default(); 1],
        };
        
        let mut bits: *mut std::ffi::c_void = std::ptr::null_mut();
        let hbitmap = CreateDIBSection(
            hdc,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits,
            HANDLE::default(),
            0,
        )?;
        
        if !bits.is_null() && rgb_data.len() >= (width * height * 3) as usize {
            std::ptr::copy_nonoverlapping(
                rgb_data.as_ptr(),
                bits as *mut u8,
                (width * height * 3) as usize,
            );
        }
        
        ReleaseDC(HWND::default(), hdc);
        Ok(hbitmap)
    }
}