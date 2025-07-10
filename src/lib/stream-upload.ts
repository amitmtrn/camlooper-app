import { invoke } from '@tauri-apps/api/core';

export interface StreamUploadRequest {
  filename: string;
  file_size: number;
  chunk_size: number;
}

export interface StreamUploadResponse {
  upload_id: string;
  temp_file_path: string;
  chunk_size: number;
  expected_chunks: number;
}

export interface ChunkUploadRequest {
  upload_id: string;
  chunk_index: number;
  chunk_data: number[]; // Array of bytes
  is_final_chunk: boolean;
}

export interface ChunkUploadResponse {
  upload_id: string;
  chunk_index: number;
  bytes_written: number;
  total_bytes_written: number;
  is_complete: boolean;
  file_path?: string;
}

export interface VideoInfo {
  id: string;
  filename: string;
  duration: number;
  width: number;
  height: number;
  fps: number;
  format: string;
}

export interface UploadProgress {
  progress: number; // 0-100
  bytesUploaded: number;
  totalBytes: number;
  chunksUploaded: number;
  totalChunks: number;
}

export class StreamUploader {
  private file: File;
  private chunkSize: number;
  private onProgress?: (progress: UploadProgress) => void;
  private onComplete?: (videoInfo: VideoInfo) => void;
  private onError?: (error: Error) => void;

  constructor(
    file: File,
    options: {
      chunkSize?: number;
      onProgress?: (progress: UploadProgress) => void;
      onComplete?: (videoInfo: VideoInfo) => void;
      onError?: (error: Error) => void;
    } = {}
  ) {
    this.file = file;
    this.chunkSize = options.chunkSize || 1024 * 1024; // 1MB chunks by default
    this.onProgress = options.onProgress;
    this.onComplete = options.onComplete;
    this.onError = options.onError;
  }

  async upload(): Promise<VideoInfo> {
    try {
      // Start the upload session
      const uploadResponse = await invoke<StreamUploadResponse>('start_stream_upload', {
        filename: this.file.name,
        fileSize: this.file.size,
        chunkSize: this.chunkSize,
      });

      const { upload_id, expected_chunks } = uploadResponse;
      let bytesUploaded = 0;

      // Upload chunks
      for (let chunkIndex = 0; chunkIndex < expected_chunks; chunkIndex++) {
        const start = chunkIndex * this.chunkSize;
        const end = Math.min(start + this.chunkSize, this.file.size);
        const chunk = this.file.slice(start, end);
        
        // Convert chunk to array buffer then to byte array
        const arrayBuffer = await chunk.arrayBuffer();
        const chunkData = Array.from(new Uint8Array(arrayBuffer));
        
        const chunkRequest: ChunkUploadRequest = {
          upload_id,
          chunk_index: chunkIndex,
          chunk_data: chunkData,
          is_final_chunk: chunkIndex === expected_chunks - 1,
        };

        const chunkResponse = await invoke<ChunkUploadResponse>('upload_chunk_stream', {
          request: chunkRequest,
        });

        bytesUploaded = chunkResponse.total_bytes_written;

        // Report progress
        if (this.onProgress) {
          this.onProgress({
            progress: (bytesUploaded / this.file.size) * 100,
            bytesUploaded,
            totalBytes: this.file.size,
            chunksUploaded: chunkIndex + 1,
            totalChunks: expected_chunks,
          });
        }

        // Check if upload is complete
        if (chunkResponse.is_complete && chunkResponse.file_path) {
          // Load the video file
          const videoInfo = await invoke<VideoInfo>('load_video_file', {
            filePath: chunkResponse.file_path,
          });

          if (this.onComplete) {
            this.onComplete(videoInfo);
          }

          return videoInfo;
        }
      }

      throw new Error('Upload completed but no file path returned');
    } catch (error) {
      const uploadError = error instanceof Error ? error : new Error(String(error));
      if (this.onError) {
        this.onError(uploadError);
      }
      throw uploadError;
    }
  }
}

// Convenience function for single-shot uploads (smaller files)
export async function uploadVideoFileStream(
  file: File,
  onProgress?: (progress: UploadProgress) => void
): Promise<VideoInfo> {
  // For smaller files, use the complete file upload
  if (file.size <= 10 * 1024 * 1024) { // 10MB or less
    const arrayBuffer = await file.arrayBuffer();
    const fileData = Array.from(new Uint8Array(arrayBuffer));
    
    if (onProgress) {
      onProgress({
        progress: 50,
        bytesUploaded: file.size / 2,
        totalBytes: file.size,
        chunksUploaded: 1,
        totalChunks: 1,
      });
    }
    
    const videoInfo = await invoke<VideoInfo>('stream_upload_and_load_video', {
      filename: file.name,
      fileData,
    });
    
    if (onProgress) {
      onProgress({
        progress: 100,
        bytesUploaded: file.size,
        totalBytes: file.size,
        chunksUploaded: 1,
        totalChunks: 1,
      });
    }
    
    return videoInfo;
  } else {
    // For larger files, use chunked upload
    const uploader = new StreamUploader(file, { onProgress });
    return await uploader.upload();
  }
}

// Legacy base64 upload function (deprecated)
export async function uploadVideoFileLegacy(
  file: File,
  onProgress?: (progress: UploadProgress) => void
): Promise<VideoInfo> {
  console.warn('uploadVideoFileLegacy is deprecated. Use uploadVideoFileStream instead for better performance.');
  
  // Convert file to base64
  const base64Data = await fileToBase64(file);
  
  if (onProgress) {
    onProgress({
      progress: 50,
      bytesUploaded: file.size / 2,
      totalBytes: file.size,
      chunksUploaded: 1,
      totalChunks: 1,
    });
  }
  
  const videoInfo = await invoke<VideoInfo>('upload_and_load_video', {
    filename: file.name,
    fileData: base64Data,
  });
  
  if (onProgress) {
    onProgress({
      progress: 100,
      bytesUploaded: file.size,
      totalBytes: file.size,
      chunksUploaded: 1,
      totalChunks: 1,
    });
  }
  
  return videoInfo;
}

// Helper function to convert file to base64 (for legacy support)
function fileToBase64(file: File): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.readAsDataURL(file);
    reader.onload = () => {
      const result = reader.result as string;
      // Remove the data URL prefix (e.g., "data:video/mp4;base64,")
      const base64 = result.split(',')[1];
      resolve(base64);
    };
    reader.onerror = reject;
  });
} 