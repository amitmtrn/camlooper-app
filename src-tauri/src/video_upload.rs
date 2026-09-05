use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{Write, BufWriter};
use std::path::PathBuf;
use std::fs::File;
use std::sync::{Mutex, OnceLock};
use tempfile::NamedTempFile;
use uuid::Uuid;

// Public API type kept for the IPC contract; not constructed internally yet.
#[allow(dead_code)]
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
        // Add better error handling for Windows
        println!("Creating upload session: {} bytes, {} byte chunks", file_size, chunk_size);
        
        let temp_file = NamedTempFile::new()
            .map_err(|e| anyhow!("Failed to create temporary file: {}. Check temp directory permissions.", e))?;
        
        let file = temp_file.reopen()
            .map_err(|e| anyhow!("Failed to reopen temporary file: {}. Check file system permissions.", e))?;
        
        let writer = BufWriter::new(file);
        
        let expected_chunks = ((file_size as f64) / (chunk_size as f64)).ceil() as usize;
        
        println!("Upload session created: temp file at {:?}, expecting {} chunks", 
            temp_file.path(), expected_chunks);
        
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
        if chunk_data.is_empty() {
            return Err(anyhow!("Cannot write empty chunk"));
        }
        
        let bytes_written = self.writer.write(chunk_data)
            .map_err(|e| anyhow!("Failed to write chunk to file: {}. Check disk space and permissions.", e))?;
        
        self.bytes_written += bytes_written as u64;
        self.chunks_received += 1;
        
        println!("Chunk {} written: {} bytes (total: {}/{})", 
            self.chunks_received, bytes_written, self.bytes_written, self.file_size);
        
        Ok(bytes_written)
    }

    pub fn is_complete(&self) -> bool {
        let complete = self.bytes_written >= self.file_size;
        if complete {
            println!("Upload complete: {} bytes written", self.bytes_written);
        }
        complete
    }

    pub fn finalize(&mut self) -> Result<()> {
        self.writer.flush()
            .map_err(|e| anyhow!("Failed to flush file data: {}. Check disk space.", e))?;
        
        println!("Upload finalized: {} chunks, {} bytes", self.chunks_received, self.bytes_written);
        Ok(())
    }

    pub fn get_file_path(&self) -> PathBuf {
        self.temp_file.path().to_path_buf()
    }
}

pub fn start_stream_upload(filename: String, file_size: u64, chunk_size: usize) -> Result<StreamUploadResponse> {
    println!("Starting stream upload: {} ({} bytes, {} byte chunks)", filename, file_size, chunk_size);
    
    if file_size == 0 {
        return Err(anyhow!("Cannot upload empty file"));
    }
    
    if chunk_size == 0 || chunk_size > 10 * 1024 * 1024 { // Max 10MB chunks
        return Err(anyhow!("Invalid chunk size: {}. Must be between 1 and 10MB", chunk_size));
    }
    
    let upload_id = Uuid::new_v4().to_string();
    let session = StreamUploadSession::new(file_size, chunk_size)
        .map_err(|e| anyhow!("Failed to create upload session for {}: {}", filename, e))?;
    
    let temp_file_path = session.get_file_path().to_string_lossy().to_string();
    let expected_chunks = session.expected_chunks;
    
    let mut sessions = get_upload_sessions().lock()
        .map_err(|e| anyhow!("Failed to acquire upload sessions lock: {}", e))?;
    sessions.insert(upload_id.clone(), session);
    
    println!("Upload session started: ID={}, temp_file={}, expected_chunks={}", 
        upload_id, temp_file_path, expected_chunks);
    
    Ok(StreamUploadResponse {
        upload_id,
        temp_file_path,
        chunk_size,
        expected_chunks,
    })
}

pub fn upload_chunk_stream(request: ChunkUploadRequest) -> Result<ChunkUploadResponse> {
    println!("Uploading chunk: ID={}, index={}, size={} bytes, final={}", 
        request.upload_id, request.chunk_index, request.chunk_data.len(), request.is_final_chunk);
    
    if request.chunk_data.is_empty() {
        return Err(anyhow!("Chunk data is empty for upload {}", request.upload_id));
    }
    
    let mut sessions = get_upload_sessions().lock()
        .map_err(|e| anyhow!("Failed to acquire upload sessions lock: {}", e))?;
    
    let session = sessions.get_mut(&request.upload_id)
        .ok_or_else(|| anyhow!("Upload session not found: {}. Session may have expired or been cleaned up.", request.upload_id))?;

    // Write chunk data directly to file
    let bytes_written = session.write_chunk(&request.chunk_data)
        .map_err(|e| anyhow!("Failed to write chunk {} for upload {}: {}", request.chunk_index, request.upload_id, e))?;
    
    let is_complete = request.is_final_chunk || session.is_complete();
    
    if is_complete {
        session.finalize()
            .map_err(|e| anyhow!("Failed to finalize upload {}: {}", request.upload_id, e))?;
        println!("Upload {} completed successfully", request.upload_id);
    }
    
    let response = ChunkUploadResponse {
        upload_id: request.upload_id.clone(),
        chunk_index: request.chunk_index,
        bytes_written,
        total_bytes_written: session.bytes_written,
        is_complete,
        file_path: if is_complete {
            let file_path = session.get_file_path().to_string_lossy().to_string();
            println!("Upload {} complete. File saved to: {}", request.upload_id, file_path);
            Some(file_path)
        } else {
            None
        },
    };

    Ok(response)
}

// Simplified single-shot upload for smaller files
pub fn upload_complete_file_stream(filename: String, file_data: Vec<u8>) -> Result<String> {
    println!("Uploading complete file: {} ({} bytes)", filename, file_data.len());
    println!("Windows upload - filename: {}, data length: {}", filename, file_data.len());
    
    if file_data.is_empty() {
        return Err(anyhow!("Cannot upload empty file: {}", filename));
    }
    
    let mut temp_file = NamedTempFile::new()
        .map_err(|e| anyhow!("Failed to create temporary file for {}: {}. Check temp directory permissions.", filename, e))?;
    
    temp_file.write_all(&file_data)
        .map_err(|e| anyhow!("Failed to write file data for {}: {}. Check disk space.", filename, e))?;
    
    temp_file.flush()
        .map_err(|e| anyhow!("Failed to flush file data for {}: {}. Check disk space.", filename, e))?;

    let file_path = temp_file.path().to_string_lossy().to_string();
    
    // Keep the temp file alive by storing it in a simple session
    let upload_id = Uuid::new_v4().to_string();
    let file_size = file_data.len() as u64;
    
    // Create a dummy writer that's safe cross-platform
    let dummy_file = NamedTempFile::new()
        .map_err(|e| anyhow!("Failed to create dummy file for session: {}", e))?;
    
    let session = StreamUploadSession {
        file_size,
        temp_file,
        writer: BufWriter::new(dummy_file.reopen().map_err(|e| anyhow!("Failed to create dummy writer: {}", e))?), // Safe dummy writer
        chunks_received: 1,
        bytes_written: file_size,
        expected_chunks: 1,
    };

    let mut sessions = get_upload_sessions().lock()
        .map_err(|e| anyhow!("Failed to acquire upload sessions lock: {}", e))?;
    sessions.insert(upload_id.clone(), session);

    println!("Complete file upload successful: {} -> {} (session: {})", filename, file_path, upload_id);
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