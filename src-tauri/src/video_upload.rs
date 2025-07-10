use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Write, BufWriter};
use std::path::PathBuf;
use std::fs::File;
use std::sync::{Mutex, OnceLock};
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUploadRequest {
    pub filename: String,
    pub file_size: u64,
    pub chunk_size: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamUploadResponse {
    pub upload_id: String,
    pub temp_file_path: String,
    pub chunk_size: usize,
    pub expected_chunks: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkUploadRequest {
    pub upload_id: String,
    pub chunk_index: usize,
    pub chunk_data: Vec<u8>, // Raw binary data instead of base64
    pub is_final_chunk: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkUploadResponse {
    pub upload_id: String,
    pub chunk_index: usize,
    pub bytes_written: usize,
    pub total_bytes_written: u64,
    pub is_complete: bool,
    pub file_path: Option<String>,
}

#[derive(Debug)]
struct StreamUploadSession {
    pub file_size: u64,
    pub temp_file: NamedTempFile,
    pub writer: BufWriter<File>,
    pub chunks_received: usize,
    pub bytes_written: u64,
    pub expected_chunks: usize,
}

static UPLOAD_SESSIONS: OnceLock<Mutex<HashMap<String, StreamUploadSession>>> = OnceLock::new();

fn get_upload_sessions() -> &'static Mutex<HashMap<String, StreamUploadSession>> {
    UPLOAD_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl StreamUploadSession {
    pub fn new(file_size: u64, chunk_size: usize) -> Result<Self> {
        let temp_file = NamedTempFile::new()?;
        let file = temp_file.reopen()?;
        let writer = BufWriter::new(file);
        
        let expected_chunks = ((file_size as f64) / (chunk_size as f64)).ceil() as usize;
        
        Ok(Self {
            file_size,
            temp_file,
            writer,
            chunks_received: 0,
            bytes_written: 0,
            expected_chunks,
        })
    }

    pub fn write_chunk(&mut self, chunk_data: &[u8]) -> Result<usize> {
        let bytes_written = self.writer.write(chunk_data)?;
        self.bytes_written += bytes_written as u64;
        self.chunks_received += 1;
        Ok(bytes_written)
    }

    pub fn is_complete(&self) -> bool {
        self.bytes_written >= self.file_size
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.writer.flush()?;
        Ok(())
    }

    pub fn get_file_path(&self) -> PathBuf {
        self.temp_file.path().to_path_buf()
    }
}

pub fn start_stream_upload(_filename: String, file_size: u64, chunk_size: usize) -> Result<StreamUploadResponse> {
    let upload_id = Uuid::new_v4().to_string();
    let session = StreamUploadSession::new(file_size, chunk_size)?;
    
    let temp_file_path = session.get_file_path().to_string_lossy().to_string();
    let expected_chunks = session.expected_chunks;
    
    let mut sessions = get_upload_sessions().lock().unwrap();
    sessions.insert(upload_id.clone(), session);
    
    Ok(StreamUploadResponse {
        upload_id,
        temp_file_path,
        chunk_size,
        expected_chunks,
    })
}

pub fn upload_chunk_stream(request: ChunkUploadRequest) -> Result<ChunkUploadResponse> {
    let mut sessions = get_upload_sessions().lock().unwrap();
    let session = sessions.get_mut(&request.upload_id)
        .ok_or_else(|| anyhow!("Upload session not found: {}", request.upload_id))?;

    // Write chunk data directly to file
    let bytes_written = session.write_chunk(&request.chunk_data)?;
    
    let is_complete = request.is_final_chunk || session.is_complete();
    
    if is_complete {
        session.finalize()?;
    }
    
    let response = ChunkUploadResponse {
        upload_id: request.upload_id.clone(),
        chunk_index: request.chunk_index,
        bytes_written,
        total_bytes_written: session.bytes_written,
        is_complete,
        file_path: if is_complete {
            Some(session.get_file_path().to_string_lossy().to_string())
        } else {
            None
        },
    };

    Ok(response)
}

// Simplified single-shot upload for smaller files
pub fn upload_complete_file_stream(_filename: String, file_data: Vec<u8>) -> Result<String> {
    let mut temp_file = NamedTempFile::new()?;
    temp_file.write_all(&file_data)?;
    temp_file.flush()?;

    let file_path = temp_file.path().to_string_lossy().to_string();
    
    // Keep the temp file alive by storing it in a simple session
    let upload_id = Uuid::new_v4().to_string();
    let file_size = file_data.len() as u64;
    
    let session = StreamUploadSession {
        file_size,
        temp_file,
        writer: BufWriter::new(File::create("/dev/null")?), // Dummy writer since file is already written
        chunks_received: 1,
        bytes_written: file_size,
        expected_chunks: 1,
    };

    let mut sessions = get_upload_sessions().lock().unwrap();
    sessions.insert(upload_id, session);

    Ok(file_path)
}

// Legacy base64 support (deprecated - use streaming instead)
#[deprecated(note = "Use streaming upload instead for better performance")]
pub fn upload_complete_file(filename: String, file_data: String) -> Result<String> {
    use base64::prelude::*;
    let file_bytes = BASE64_STANDARD.decode(&file_data)
        .map_err(|e| anyhow!("Failed to decode file data: {}", e))?;
    
    upload_complete_file_stream(filename, file_bytes)
}

pub fn cleanup_upload(upload_id: &str) -> Result<()> {
    let mut sessions = get_upload_sessions().lock().unwrap();
    if sessions.remove(upload_id).is_some() {
        println!("Cleaned up upload session: {}", upload_id);
    }
    Ok(())
}

pub fn get_upload_progress(upload_id: &str) -> Result<ChunkUploadResponse> {
    let sessions = get_upload_sessions().lock().unwrap();
    let session = sessions.get(upload_id)
        .ok_or_else(|| anyhow!("Upload session not found: {}", upload_id))?;

    let is_complete = session.is_complete();
    
    Ok(ChunkUploadResponse {
        upload_id: upload_id.to_string(),
        chunk_index: session.chunks_received,
        bytes_written: 0, // Current chunk bytes written
        total_bytes_written: session.bytes_written,
        is_complete,
        file_path: if is_complete {
            Some(session.get_file_path().to_string_lossy().to_string())
        } else {
            None
        },
    })
}

// Helper function to validate video file types
pub fn is_supported_video_format(filename: &str) -> bool {
    let extension = std::path::Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_lowercase();

    matches!(extension.as_str(), "mp4" | "mov" | "avi" | "mkv" | "webm" | "flv" | "wmv" | "m4v")
}

// Backward compatibility types (deprecated)
#[allow(deprecated)]
mod deprecated_types {
    use super::*;

    #[deprecated(note = "Use StreamUploadRequest instead")]
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UploadRequest {
        pub filename: String,
        pub file_data: String, // Base64 encoded file data
        pub chunk_index: usize,
        pub total_chunks: usize,
        pub upload_id: Option<String>,
    }

    #[deprecated(note = "Use ChunkUploadResponse instead")]
    #[derive(Debug, Clone, Serialize, Deserialize)]
    pub struct UploadResponse {
        pub upload_id: String,
        pub chunk_received: usize,
        pub total_chunks: usize,
        pub is_complete: bool,
        pub file_path: Option<String>,
    }
}

// Re-export deprecated types for backward compatibility
#[allow(deprecated)]
pub use deprecated_types::{UploadRequest, UploadResponse};

// Backward compatibility functions (deprecated)
#[deprecated(note = "Use start_stream_upload instead")]
#[allow(deprecated)]
pub fn start_upload(filename: String, total_chunks: usize) -> Result<String> {
    // Fallback implementation - estimate file size
    let estimated_file_size = 50 * 1024 * 1024; // 50MB default
    let chunk_size = estimated_file_size / total_chunks as u64;
    
    let response = start_stream_upload(filename, estimated_file_size, chunk_size as usize)?;
    Ok(response.upload_id)
}

#[deprecated(note = "Use upload_chunk_stream instead")]
#[allow(deprecated)]
pub fn upload_chunk(upload_request: UploadRequest) -> Result<UploadResponse> {
    use base64::prelude::*;
    
    let upload_id = upload_request.upload_id
        .ok_or_else(|| anyhow!("Upload ID is required for chunk upload"))?;

    // Decode base64 data
    let chunk_data = BASE64_STANDARD.decode(&upload_request.file_data)
        .map_err(|e| anyhow!("Failed to decode chunk data: {}", e))?;

    let stream_request = ChunkUploadRequest {
        upload_id,
        chunk_index: upload_request.chunk_index,
        chunk_data,
        is_final_chunk: upload_request.chunk_index == upload_request.total_chunks - 1,
    };

    let stream_response = upload_chunk_stream(stream_request)?;
    
    Ok(UploadResponse {
        upload_id: stream_response.upload_id,
        chunk_received: stream_response.chunk_index,
        total_chunks: upload_request.total_chunks,
        is_complete: stream_response.is_complete,
        file_path: stream_response.file_path,
    })
}

#[deprecated(note = "Use get_upload_progress instead")]
#[allow(deprecated)]
pub fn get_upload_status(upload_id: &str) -> Result<UploadResponse> {
    let progress = get_upload_progress(upload_id)?;
    
    Ok(UploadResponse {
        upload_id: progress.upload_id,
        chunk_received: progress.chunk_index,
        total_chunks: 1, // Unknown in new system
        is_complete: progress.is_complete,
        file_path: progress.file_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stream_upload_session_creation() {
        let session = StreamUploadSession::new(1024, 256).unwrap();
        assert_eq!(session.file_size, 1024);
        assert_eq!(session.expected_chunks, 4);
    }

    #[test]
    fn test_chunk_calculation() {
        let file_size = 1000u64;
        let chunk_size = 300usize;
        let expected_chunks = ((file_size as f64) / (chunk_size as f64)).ceil() as usize;
        assert_eq!(expected_chunks, 4);
    }
} 