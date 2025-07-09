// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod virtual_camera;

use virtual_camera::{VirtualCameraConfig, VirtualCameraStatus};
use serde::Deserialize;

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

#[tauri::command]
async fn start_virtual_camera(config: Option<VirtualCameraConfig>) -> Result<VirtualCameraStatus, String> {
    virtual_camera::start_virtual_camera(config)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn stop_virtual_camera() -> Result<VirtualCameraStatus, String> {
    virtual_camera::stop_virtual_camera()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_virtual_camera_status() -> VirtualCameraStatus {
    virtual_camera::get_virtual_camera_status().await
}

#[derive(Deserialize)]
struct FrameData {
    data: String, // Base64 encoded frame data
}

#[tauri::command]
async fn send_frame_to_virtual_camera(frame_data: String) -> Result<(), String> {
    // Decode base64 frame data
    use base64::prelude::*;
    let decoded_data = BASE64_STANDARD.decode(&frame_data)
        .map_err(|e| format!("Failed to decode frame data: {}", e))?;
    
    virtual_camera::send_frame_to_virtual_camera(decoded_data)
        .await
        .map_err(|e| e.to_string())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            start_virtual_camera,
            stop_virtual_camera,
            get_virtual_camera_status,
            send_frame_to_virtual_camera
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
