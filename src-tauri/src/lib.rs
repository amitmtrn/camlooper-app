// Learn more about Tauri commands at https://tauri.app/develop/calling-rust/
mod virtual_camera;
mod video_processor;
mod video_upload;

use virtual_camera::{VirtualCameraConfig, VirtualCameraStatus};
use video_processor::{VideoInfo, StreamStatus, PerformanceMetrics};

#[allow(deprecated)]
use video_upload::{
    UploadRequest, UploadResponse, // Legacy types
    StreamUploadResponse, ChunkUploadRequest, ChunkUploadResponse // New streaming types
};

#[tauri::command]
fn greet(name: &str) -> String {
    format!("Hello, {}! You've been greeted from Rust!", name)
}

// Virtual Camera Commands
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

// New Streaming Video Upload Commands
#[tauri::command]
async fn start_stream_upload(filename: String, file_size: u64, chunk_size: usize) -> Result<StreamUploadResponse, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    video_upload::start_stream_upload(filename, file_size, chunk_size)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn upload_chunk_stream(request: ChunkUploadRequest) -> Result<ChunkUploadResponse, String> {
    video_upload::upload_chunk_stream(request)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn upload_complete_file_stream(filename: String, file_data: Vec<u8>) -> Result<String, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    video_upload::upload_complete_file_stream(filename, file_data)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_upload_progress(upload_id: String) -> Result<ChunkUploadResponse, String> {
    video_upload::get_upload_progress(&upload_id)
        .map_err(|e| e.to_string())
}

// Legacy Video Upload Commands (deprecated but maintained for backward compatibility)
#[tauri::command]
async fn start_video_upload(filename: String, total_chunks: usize) -> Result<String, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    #[allow(deprecated)]
    video_upload::start_upload(filename, total_chunks)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(deprecated)]
async fn upload_video_chunk(upload_request: UploadRequest) -> Result<UploadResponse, String> {
    video_upload::upload_chunk(upload_request)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn upload_complete_video_file(filename: String, file_data: String) -> Result<String, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    #[allow(deprecated)]
    video_upload::upload_complete_file(filename, file_data)
        .map_err(|e| e.to_string())
}

#[tauri::command]
#[allow(deprecated)]
async fn get_upload_status(upload_id: String) -> Result<UploadResponse, String> {
    video_upload::get_upload_status(&upload_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn cleanup_video_upload(upload_id: String) -> Result<(), String> {
    video_upload::cleanup_upload(&upload_id)
        .map_err(|e| e.to_string())
}

// Video Processing Commands
#[tauri::command]
async fn load_video_file(file_path: String) -> Result<VideoInfo, String> {
    video_processor::load_video_file(file_path)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn start_video_stream() -> Result<(), String> {
    video_processor::start_video_stream()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn pause_video_stream() -> Result<(), String> {
    video_processor::pause_video_stream()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn stop_video_stream() -> Result<(), String> {
    video_processor::stop_video_stream()
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_video_stream_status() -> StreamStatus {
    video_processor::get_video_stream_status().await
}

#[tauri::command]
async fn set_video_loop_settings(loop_count: u32, auto_start: bool) -> Result<(), String> {
    video_processor::set_video_loop_settings(loop_count, auto_start)
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_video_info() -> Option<VideoInfo> {
    video_processor::get_video_info().await
}

#[tauri::command]
async fn get_performance_metrics() -> Result<PerformanceMetrics, String> {
    video_processor::get_performance_metrics()
        .await
        .map_err(|e| e.to_string())
}

// New efficient streaming workflow command
#[tauri::command]
async fn stream_upload_and_load_video(filename: String, file_data: Vec<u8>) -> Result<VideoInfo, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    // Upload the file using streaming (no base64 conversion)
    let file_path = video_upload::upload_complete_file_stream(filename.clone(), file_data)
        .map_err(|e| format!("Streaming upload failed: {}", e))?;
    
    // Load the video
    let video_info = video_processor::load_video_file(file_path)
        .await
        .map_err(|e| format!("Video loading failed: {}", e))?;
    
    Ok(video_info)
}

// Combined workflow command for uploading and loading a video (legacy - base64 based)
#[tauri::command]
async fn upload_and_load_video(filename: String, file_data: String) -> Result<VideoInfo, String> {
    if !video_upload::is_supported_video_format(&filename) {
        return Err("Unsupported video format".to_string());
    }
    
    // Upload the file using legacy base64 method
    #[allow(deprecated)]
    let file_path = video_upload::upload_complete_file(filename.clone(), file_data)
        .map_err(|e| format!("Upload failed: {}", e))?;
    
    // Load the video
    let video_info = video_processor::load_video_file(file_path)
        .await
        .map_err(|e| format!("Video loading failed: {}", e))?;
    
    Ok(video_info)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_fs::init())
        .invoke_handler(tauri::generate_handler![
            greet,
            // Virtual Camera
            start_virtual_camera,
            stop_virtual_camera,
            get_virtual_camera_status,
            send_frame_to_virtual_camera,
            // New Streaming Upload Commands
            start_stream_upload,
            upload_chunk_stream,
            upload_complete_file_stream,
            get_upload_progress,
            // Legacy Video Upload Commands (deprecated)
            start_video_upload,
            upload_video_chunk,
            upload_complete_video_file,
            get_upload_status,
            cleanup_video_upload,
            // Video Processing
            load_video_file,
            start_video_stream,
            pause_video_stream,
            stop_video_stream,
            get_video_stream_status,
            set_video_loop_settings,
            get_video_info,
            get_performance_metrics,
            // Combined workflow commands
            stream_upload_and_load_video, // New efficient streaming command
            upload_and_load_video         // Legacy base64 command
        ])
        .setup(|app| {
            // Initialize video processor with app handle for push-based frame streaming
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                video_processor::init_video_processor(app_handle).await;
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
