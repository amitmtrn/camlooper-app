use anyhow::{anyhow, Result};
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use tempfile::NamedTempFile;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadRequest {
    pub filename: String,
    pub file_data: String, // Base64 encoded file data
    pub chunk_index: usize,
    pub total_chunks: usize,
    pub upload_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResponse {
    pub upload_id: String,
    pub chunk_received: usize,
    pub total_chunks: usize,
    pub is_complete: bool,
    pub file_path: Option<String>,
}

#[derive(Debug)]
struct UploadSession {
    pub filename: String,
    pub temp_file: NamedTempFile,
    pub chunks_received: usize,
    pub total_chunks: usize,
    pub chunk_data: HashMap<usize, Vec<u8>>,
}

static mut UPLOAD_SESSIONS: Option<HashMap<String, UploadSession>> = None;
static INIT: std::sync::Once = std::sync::Once::new();

fn get_upload_sessions() -> &'static mut HashMap<String, UploadSession> {
    unsafe {
        INIT.call_once(|| {
            UPLOAD_SESSIONS = Some(HashMap::new());
        });
        UPLOAD_SESSIONS.as_mut().unwrap()
    }
}

impl UploadSession {
    pub fn new(filename: String, total_chunks: usize) -> Result<Self> {
        let temp_file = NamedTempFile::new()?;
        
        Ok(Self {
            filename,
            temp_file,
            chunks_received: 0,
            total_chunks,
            chunk_data: HashMap::new(),
        })
    }

    pub fn add_chunk(&mut self, chunk_index: usize, data: Vec<u8>) -> Result<bool> {
        if chunk_index >= self.total_chunks {
            return Err(anyhow!("Chunk index {} exceeds total chunks {}", chunk_index, self.total_chunks));
        }

        if self.chunk_data.contains_key(&chunk_index) {
            return Err(anyhow!("Chunk {} already received", chunk_index));
        }

        self.chunk_data.insert(chunk_index, data);
        self.chunks_received += 1;

        // Check if all chunks are received
        if self.chunks_received == self.total_chunks {
            self.finalize_file()?;
            return Ok(true);
        }

        Ok(false)
    }

    fn finalize_file(&mut self) -> Result<()> {
        // Write all chunks in order to the temp file
        for i in 0..self.total_chunks {
            if let Some(chunk_data) = self.chunk_data.get(&i) {
                self.temp_file.write_all(chunk_data)?;
            } else {
                return Err(anyhow!("Missing chunk {}", i));
            }
        }
        
        self.temp_file.flush()?;
        Ok(())
    }

    pub fn get_file_path(&self) -> PathBuf {
        self.temp_file.path().to_path_buf()
    }
}

pub fn start_upload(filename: String, total_chunks: usize) -> Result<String> {
    let upload_id = Uuid::new_v4().to_string();
    let session = UploadSession::new(filename, total_chunks)?;
    
    let sessions = get_upload_sessions();
    sessions.insert(upload_id.clone(), session);
    
    Ok(upload_id)
}

pub fn upload_chunk(upload_request: UploadRequest) -> Result<UploadResponse> {
    let upload_id = upload_request.upload_id
        .ok_or_else(|| anyhow!("Upload ID is required for chunk upload"))?;

    // Decode base64 data
    let chunk_data = base64::prelude::BASE64_STANDARD.decode(&upload_request.file_data)
        .map_err(|e| anyhow!("Failed to decode chunk data: {}", e))?;

    let sessions = get_upload_sessions();
    let session = sessions.get_mut(&upload_id)
        .ok_or_else(|| anyhow!("Upload session not found: {}", upload_id))?;

    let is_complete = session.add_chunk(upload_request.chunk_index, chunk_data)?;
    
    let response = UploadResponse {
        upload_id: upload_id.clone(),
        chunk_received: upload_request.chunk_index,
        total_chunks: session.total_chunks,
        is_complete,
        file_path: if is_complete {
            Some(session.get_file_path().to_string_lossy().to_string())
        } else {
            None
        },
    };

    Ok(response)
}

pub fn upload_complete_file(filename: String, file_data: String) -> Result<String> {
    // For single file upload (no chunking)
    let file_bytes = base64::prelude::BASE64_STANDARD.decode(&file_data)
        .map_err(|e| anyhow!("Failed to decode file data: {}", e))?;

    let mut temp_file = NamedTempFile::new()?;
    temp_file.write_all(&file_bytes)?;
    temp_file.flush()?;

    // Get the temp file path before the file is potentially moved
    let file_path = temp_file.path().to_string_lossy().to_string();
    
    // Keep the temp file alive by storing it
    let upload_id = Uuid::new_v4().to_string();
    let session = UploadSession {
        filename,
        temp_file,
        chunks_received: 1,
        total_chunks: 1,
        chunk_data: HashMap::new(),
    };

    let sessions = get_upload_sessions();
    sessions.insert(upload_id.clone(), session);

    Ok(file_path)
}

pub fn cleanup_upload(upload_id: &str) -> Result<()> {
    let sessions = get_upload_sessions();
    if sessions.remove(upload_id).is_some() {
        println!("Cleaned up upload session: {}", upload_id);
    }
    Ok(())
}

pub fn get_upload_status(upload_id: &str) -> Result<UploadResponse> {
    let sessions = get_upload_sessions();
    let session = sessions.get(upload_id)
        .ok_or_else(|| anyhow!("Upload session not found: {}", upload_id))?;

    let is_complete = session.chunks_received == session.total_chunks;
    
    Ok(UploadResponse {
        upload_id: upload_id.to_string(),
        chunk_received: session.chunks_received,
        total_chunks: session.total_chunks,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supported_video_formats() {
        assert!(is_supported_video_format("test.mp4"));
        assert!(is_supported_video_format("test.MP4"));
        assert!(is_supported_video_format("test.mov"));
        assert!(is_supported_video_format("test.avi"));
        assert!(!is_supported_video_format("test.txt"));
        assert!(!is_supported_video_format("test.jpg"));
    }

    #[test]
    fn test_upload_session_creation() {
        let session = UploadSession::new("test.mp4".to_string(), 3);
        assert!(session.is_ok());
        
        let session = session.unwrap();
        assert_eq!(session.filename, "test.mp4");
        assert_eq!(session.total_chunks, 3);
        assert_eq!(session.chunks_received, 0);
    }
} 