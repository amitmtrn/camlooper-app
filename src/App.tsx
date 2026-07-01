import { Toaster } from "@/components/ui/toaster";
import { Toaster as Sonner } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import { 
  Play, 
  Pause, 
  Square, 
  Upload, 
  Settings, 
  Monitor, 
  Video, 
  RotateCcw,
  FileText,
  Download,
  Trash2,
  Eye,
  EyeOff,
  Radio,
  Circle
} from "lucide-react";
import React, { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { VideoRecorder } from "./components/VideoRecorder";
import { AdBanner } from "./components/AdBanner";
import { useAdPopup } from "./hooks/use-ad-popup";

const queryClient = new QueryClient();

// Types for virtual camera
interface VirtualCameraStatus {
  is_active: boolean;
  camera_name: string;
  resolution: string;
  fps: number;
  frame_count: number;
}

interface VirtualCameraConfig {
  width: number;
  height: number;
  fps: number;
  camera_name: string;
}

// Types for video processing
interface VideoInfo {
  id: string;
  filename: string;
  duration: number;
  width: number;
  height: number;
  fps: number;
  format: string;
}

interface VideoFrame {
  data: number[]; // Raw JPEG bytes as array
  timestamp: number;
  width: number;
  height: number;
}

interface FrameBatch {
  frames: VideoFrame[];
  sequence_id: number;
  total_frames: number;
}

interface StreamStatus {
  is_playing: boolean;
  current_time: number;
  duration: number;
  loop_count: number;
  current_loop: number;
  buffer_health: number; // 0.0 to 1.0
  actual_fps: number;
  target_fps: number;
  frames_dropped: number;
  average_processing_time: number;
}

interface PerformanceMetrics {
  frames_processed: number;
  frames_dropped: number;
  average_encode_time: number;
  average_decode_time: number;
  current_quality: number;
  buffer_size: number;
  memory_usage: number;
}

// Simple frame holder for immediate playback
class SimpleFrameHolder {
  private latestFrame: VideoFrame | null = null;
  private isPlaying: boolean = false;

  setLatestFrame(frame: VideoFrame) {
    this.latestFrame = frame;
  }

  getLatestFrame(): VideoFrame | null {
    return this.isPlaying ? this.latestFrame : null;
  }

  clear() {
    this.latestFrame = null;
  }

  start() {
    this.isPlaying = true;
  }

  stop() {
    this.isPlaying = false;
  }
}

function CamLooper() {
  useAdPopup();
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVirtualCamActive, setIsVirtualCamActive] = useState(false);
  const [loopCount, setLoopCount] = useState([10]);
  const [selectedVideo, setSelectedVideo] = useState<File | null>(null);
  const [videoInfo, setVideoInfo] = useState<VideoInfo | null>(null);
  const [currentFrame, setCurrentFrame] = useState<VideoFrame | null>(null);
  const [videoMetadata, setVideoMetadata] = useState({
    name: "No Video Selected",
    duration: "0:00",
    dimensions: "",
    fps: ""
  });
  const [streamStatus, setStreamStatus] = useState<StreamStatus>({
    is_playing: false,
    current_time: 0,
    duration: 0,
    loop_count: 10,
    current_loop: 0,
    buffer_health: 0.0,
    actual_fps: 0.0,
    target_fps: 30.0,
    frames_dropped: 0,
    average_processing_time: 0.0
  });
  const [performanceMetrics, setPerformanceMetrics] = useState<PerformanceMetrics>({
    frames_processed: 0,
    frames_dropped: 0,
    average_encode_time: 0.0,
    average_decode_time: 0.0,
    current_quality: 85,
    buffer_size: 0,
    memory_usage: 0
  });

  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState(0);
  const [autoStart, setAutoStart] = useState(true);
  const [isLoopingComplete, setIsLoopingComplete] = useState(false);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [logs, setLogs] = useState<string[]>([]);
  const [showLogs, setShowLogs] = useState(false);
  const [virtualCameraStatus, setVirtualCameraStatus] = useState<VirtualCameraStatus>({
    is_active: false,
    camera_name: "CamLooper Virtual Camera",
    resolution: "1920x1080",
    fps: 30,
    frame_count: 0
  });
  const [isVirtualCamLoading, setIsVirtualCamLoading] = useState(false);
  const [showPerformanceMetrics, setShowPerformanceMetrics] = useState(false);

  // Live camera passthrough (camera -> app -> virtual camera -> Zoom)
  const [isLiveCamera, setIsLiveCamera] = useState(false);
  const [isLiveLoading, setIsLiveLoading] = useState(false);
  const [isLiveRecording, setIsLiveRecording] = useState(false);
  const [lastRecordingPath, setLastRecordingPath] = useState<string | null>(null);
  const [liveDevices, setLiveDevices] = useState<string[]>([]);
  const [selectedLiveDevice, setSelectedLiveDevice] = useState<string>("");
  const [liveFrame, setLiveFrame] = useState<string | null>(null);

  // Simple frame holder
  const frameHolder = React.useRef(new SimpleFrameHolder());
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const batchListenerRef = React.useRef<(() => void) | null>(null);
  const liveListenerRef = React.useRef<(() => void) | null>(null);
  const logTextareaRef = React.useRef<HTMLTextAreaElement>(null);

  // Log management
  const addLog = (message: string) => {
    const timestamp = new Date().toLocaleTimeString();
    const logEntry = `[${timestamp}] ${message}`;
    setLogs(prev => {
      const newLogs = [...prev, logEntry];
      // Keep only last 100 entries to prevent memory issues
      return newLogs.slice(-100);
    });
    
    // Auto-scroll to bottom
    setTimeout(() => {
      if (logTextareaRef.current) {
        logTextareaRef.current.scrollTop = logTextareaRef.current.scrollHeight;
      }
    }, 100);
  };

  const clearLogs = () => {
    setLogs([]);
  };

  const exportLogs = () => {
    const logText = logs.join('\n');
    const blob = new Blob([logText], { type: 'text/plain' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `camlooper-logs-${new Date().toISOString().split('T')[0]}.txt`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  // Override console.log to capture logs
  React.useEffect(() => {
    const originalLog = console.log;
    const originalError = console.error;
    const originalWarn = console.warn;

    console.log = (...args) => {
      originalLog(...args);
      addLog(`LOG: ${args.join(' ')}`);
    };

    console.error = (...args) => {
      originalError(...args);
      addLog(`ERROR: ${args.join(' ')}`);
    };

    console.warn = (...args) => {
      originalWarn(...args);
      addLog(`WARN: ${args.join(' ')}`);
    };

    // Add initial log entry
    addLog('CamLooper application started - logging enabled');

    return () => {
      console.log = originalLog;
      console.error = originalError;
      console.warn = originalWarn;
    };
  }, []);

  const processVideoFile = async (file: File) => {
    if (file && file.type.startsWith('video/')) {
      console.log('File selected:', file.name, 'Size:', file.size, 'Type:', file.type);
      setSelectedVideo(file);
      setIsUploading(true);
      setUploadProgress(0);
      setUploadError(null); // Clear any previous error
      
      try {
        // Use efficient streaming upload (no base64 conversion)
        const { uploadVideoFileStream } = await import('./lib/stream-upload');
        
        console.log('Starting video upload...');
        const videoInfo = await uploadVideoFileStream(file, (progress) => {
          console.log('Upload progress:', progress.progress + '%');
          setUploadProgress(progress.progress);
        });
        
        console.log('Video uploaded successfully:', videoInfo);
        setVideoInfo(videoInfo);
        
        // Update metadata display
        const minutes = Math.floor(videoInfo.duration / 60);
        const seconds = Math.floor(videoInfo.duration % 60);
        const formattedDuration = `${minutes}:${seconds.toString().padStart(2, '0')}`;
        
        setVideoMetadata({
          name: videoInfo.filename,
          duration: formattedDuration,
          dimensions: `${videoInfo.width}x${videoInfo.height}`,
          fps: `${Math.round(videoInfo.fps)}fps`
        });
        
        console.log('Video metadata updated:', {
          name: videoInfo.filename,
          duration: formattedDuration,
          dimensions: `${videoInfo.width}x${videoInfo.height}`,
          fps: `${Math.round(videoInfo.fps)}fps`
        });
        
        // Clear any previous error on success
        setUploadError(null);
        
        // Reset state
        setIsPlaying(false);
        setIsLoopingComplete(false);
        
        // Auto start if enabled
        if (autoStart) {
          console.log('Auto-starting video playback...');
          await handlePlayPause();
        }
        
      } catch (error) {
        console.error('Error uploading video:', error);
        // Set error message
        setUploadError(error instanceof Error ? error.message : 'Failed to upload video');
        // Reset on error
        setSelectedVideo(null);
        setVideoInfo(null);
        // Reset metadata display back to initial state
        setVideoMetadata({
          name: "No Video Selected",
          duration: "0:00",
          dimensions: "",
          fps: ""
        });
      } finally {
        setIsUploading(false);
        setUploadProgress(100);
      }
    } else {
      console.log('Invalid file selected or no file selected');
    }
  };

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file) {
      await processVideoFile(file);
    }
  };

  const triggerFileInput = () => {
    const fileInput = document.getElementById('video-file-input') as HTMLInputElement;
    fileInput?.click();
  };

  const handleDrop = (event: React.DragEvent<HTMLDivElement>) => {
    event.preventDefault();
    const files = event.dataTransfer.files;
    const file = files[0];
    
    if (file) {
      processVideoFile(file);
    }
  };

  const handleDragOver = (event: React.DragEvent<HTMLDivElement>) => {
    event.preventDefault();
  };

  const handlePlayPause = async () => {
    if (!videoInfo) {
      return;
    }

    try {
      if (isPlaying) {
        await invoke('pause_video_stream');
        setIsPlaying(false);
        frameHolder.current.stop();
      } else {
        // Mutual exclusion: video playback and live camera share the virtual
        // camera sink, so stop the live camera before starting playback.
        if (isLiveCamera) {
          await stopLiveCamera();
        }
        await invoke('start_video_stream');
        setIsPlaying(true);
        await startFrameReceiving();
      }
    } catch (error) {
      console.error('Error with video playback:', error);
      setIsPlaying(false);
    }
  };

  const handleStop = async () => {
    if (!videoInfo) return;
    
    try {
      await invoke('stop_video_stream');
      setIsPlaying(false);
      frameHolder.current.stop();
      setCurrentFrame(null);
    } catch (error) {
      console.error('Error stopping video:', error);
    }
  };

  const handleRestart = async () => {
    if (!videoInfo) return;
    
    try {
      if (isLiveCamera) {
        await stopLiveCamera();
      }
      await invoke('stop_video_stream');
      await invoke('start_video_stream');
      setIsPlaying(true);
      await startFrameReceiving();
    } catch (error) {
      console.error('Error restarting video:', error);
    }
  };

      const startFrameReceiving = async () => {
      // Stop any existing listener
      if (batchListenerRef.current) {
        batchListenerRef.current();
        batchListenerRef.current = null;
      }
      
      frameHolder.current.start();
      
      // Setup pulling-based video frame loop
      let isActive = true;
      batchListenerRef.current = () => { isActive = false; };
      
      const pollFrames = async () => {
        while (isActive) {
          try {
            const batch = await invoke<FrameBatch | null>('get_video_frame_batch');
            
            if (batch) {
              console.log('Received video frame batch:', batch.frames.length, 'frames, sequence:', batch.sequence_id);
              
              // Handle test event
              if (batch.sequence_id === 999) {
                console.log('Received test event - event system is working!');
                continue;
              }
              
              // Display frames immediately as they arrive
              if (batch.frames.length > 0) {
                // Use the latest frame from the batch
                const latestFrame = batch.frames[batch.frames.length - 1];
                frameHolder.current.setLatestFrame(latestFrame);
                setCurrentFrame(latestFrame);
                // console.log('Displaying frame with timestamp:', latestFrame.timestamp);
              }
              
              // Fast poll if we got frames
              await new Promise(resolve => setTimeout(resolve, 10));
            } else {
              // Wait slightly longer if no frames (e.g. paused or queue empty)
              await new Promise(resolve => setTimeout(resolve, 33));
            }
          } catch (error) {
            console.error('Error getting video frame batch:', error);
            await new Promise(resolve => setTimeout(resolve, 100));
          }
        }
      };
      
      pollFrames();
      console.log('Video frame polling loop started successfully');
    };

  // Update stream status periodically
  useEffect(() => {
    if (!videoInfo) return;

    const statusInterval = setInterval(async () => {
      try {
        const status = await invoke<StreamStatus>('get_video_stream_status');
        setStreamStatus(status);
        
        // Update performance metrics periodically
        if (showPerformanceMetrics) {
          const metrics = await invoke<PerformanceMetrics>('get_performance_metrics');
          setPerformanceMetrics(metrics);
        }
        
        // Update virtual camera status periodically to reflect frame count
        updateVirtualCameraStatus();
        
        if (!status.is_playing && isPlaying) {
          setIsPlaying(false);
          frameHolder.current.stop();
          
          // Check if looping is complete
          if (status.current_loop >= status.loop_count && status.loop_count !== 10) {
            setIsLoopingComplete(true);
            setTimeout(() => setIsLoopingComplete(false), 3000);
          }
        }
      } catch (error) {
        console.error('Error getting stream status:', error);
      }
    }, 100); // Optimized polling frequency for better performance tracking

    return () => clearInterval(statusInterval);
  }, [videoInfo, isPlaying, showPerformanceMetrics]);

  // Update loop settings when changed
  useEffect(() => {
    if (videoInfo) {
      invoke('set_video_loop_settings', {
        loopCount: loopCount[0] === 10 ? 0 : loopCount[0],
        autoStart
      }).catch(console.error);
    }
  }, [loopCount, autoStart, videoInfo]);

  // Virtual camera functions
  const updateVirtualCameraStatus = async () => {
    try {
      const status = await invoke<VirtualCameraStatus>('get_virtual_camera_status');
      setVirtualCameraStatus(status);
      setIsVirtualCamActive(status.is_active);
    } catch (error) {
      console.error('Failed to get virtual camera status:', error);
    }
  };

  const startVirtualCamera = async () => {
    setIsVirtualCamLoading(true);
    try {
      const config: VirtualCameraConfig = {
        width: 1920,
        height: 1080,
        fps: 30,
        camera_name: "CamLooper Virtual Camera"
      };
      
      const status = await invoke<VirtualCameraStatus>('start_virtual_camera', { config });
      setVirtualCameraStatus(status);
      setIsVirtualCamActive(status.is_active);
      console.log('Virtual camera started successfully');
    } catch (error) {
      console.error('Failed to start virtual camera:', error);
      setIsVirtualCamActive(false);
    } finally {
      setIsVirtualCamLoading(false);
    }
  };

  const stopVirtualCamera = async () => {
    setIsVirtualCamLoading(true);
    try {
      const status = await invoke<VirtualCameraStatus>('stop_virtual_camera');
      setVirtualCameraStatus(status);
      setIsVirtualCamActive(status.is_active);
      console.log('Virtual camera stopped successfully');
    } catch (error) {
      console.error('Failed to stop virtual camera:', error);
    } finally {
      setIsVirtualCamLoading(false);
    }
  };

  const handleVirtualCameraToggle = async (checked: boolean) => {
    if (checked) {
      await startVirtualCamera();
    } else {
      await stopVirtualCamera();
    }
  };

  // Initialize virtual camera status
  React.useEffect(() => {
    updateVirtualCameraStatus();
  }, []);

  // --- Live camera passthrough (camera -> app -> virtual camera -> Zoom) ---

  // Enumerate physical camera devices, excluding the CamLooper virtual/loopback
  // device so the user can't feed the virtual camera into itself.
  const refreshLiveDevices = async () => {
    try {
      const devs = await invoke<string[]>('list_video_devices');
      const filtered = devs.filter(d => !d.toLowerCase().includes('camlooper'));
      setLiveDevices(filtered);
      if (filtered.length > 0 && !selectedLiveDevice) {
        setSelectedLiveDevice(filtered[0].split(':')[0]);
      }
    } catch (error) {
      console.error('Failed to list video devices:', error);
    }
  };

  React.useEffect(() => {
    refreshLiveDevices();
  }, []);

  // Poll the latest camera preview frame for the in-app preview. The same
  // frame is already being forwarded to the virtual camera by the backend.
  const startLivePreviewLoop = () => {
    if (liveListenerRef.current) {
      liveListenerRef.current();
      liveListenerRef.current = null;
    }
    let isActive = true;
    liveListenerRef.current = () => { isActive = false; };

    const pollFrames = async () => {
      while (isActive) {
        try {
          const frame = await invoke<string>('get_camera_preview_frame');
          setLiveFrame(`data:image/jpeg;base64,${frame}`);
        } catch (e) {
          // Ignore "No frame available" while ffmpeg is still starting
        }
        await new Promise(r => setTimeout(r, 1000 / 30));
      }
    };
    pollFrames();
  };

  const stopLivePreviewLoop = () => {
    if (liveListenerRef.current) {
      liveListenerRef.current();
      liveListenerRef.current = null;
    }
  };

  const startLiveCamera = async () => {
    setIsLiveLoading(true);
    try {
      // Mutual exclusion: live camera and video playback share the virtual
      // camera sink, so stop any playback first.
      if (isPlaying) {
        await handleStop();
      }
      // Ensure the virtual camera is running so frames reach Zoom.
      if (!isVirtualCamActive) {
        await startVirtualCamera();
      }
      await invoke('start_camera_preview', { deviceId: selectedLiveDevice || null });
      startLivePreviewLoop();
      setIsLiveCamera(true);
      console.log('Live camera started');
    } catch (error) {
      console.error('Failed to start live camera:', error);
      setIsLiveCamera(false);
    } finally {
      setIsLiveLoading(false);
    }
  };

  const stopLiveCamera = async () => {
    setIsLiveLoading(true);
    try {
      if (isLiveRecording) {
        await stopLiveRecording();
      }
      stopLivePreviewLoop();
      await invoke('stop_camera_preview');
      setLiveFrame(null);
      setIsLiveCamera(false);
      console.log('Live camera stopped');
    } catch (error) {
      console.error('Failed to stop live camera:', error);
    } finally {
      setIsLiveLoading(false);
    }
  };

  const handleLiveCameraToggle = async (checked: boolean) => {
    if (checked) {
      await startLiveCamera();
    } else {
      await stopLiveCamera();
    }
  };

  // Fork the live capture to a WebM file while it keeps streaming to Zoom.
  const startLiveRecording = async () => {
    try {
      await invoke('start_camera_recording', { deviceId: selectedLiveDevice || null });
      setIsLiveRecording(true);
      console.log('Live recording started (still streaming to virtual camera)');
    } catch (error) {
      console.error('Failed to start live recording:', error);
    }
  };

  const stopLiveRecording = async () => {
    try {
      const path = await invoke<string>('stop_camera_recording');
      setLastRecordingPath(path);
      setIsLiveRecording(false);
      // Resume the stream-only feed so Zoom isn't interrupted beyond the stop.
      await invoke('start_camera_preview', { deviceId: selectedLiveDevice || null });
      console.log('Live recording saved:', path);
    } catch (error) {
      console.error('Failed to stop live recording:', error);
      setIsLiveRecording(false);
    }
  };

  const handleLiveRecordToggle = async () => {
    if (isLiveRecording) {
      await stopLiveRecording();
    } else {
      await startLiveRecording();
    }
  };

  // Load a finished live recording as the loop source video.
  const useLiveRecordingAsSource = async () => {
    if (!lastRecordingPath) return;
    try {
      if (isLiveCamera) {
        await stopLiveCamera();
      }
      const base64 = await invoke<string>('get_recorded_video_base64', { path: lastRecordingPath });
      const binaryString = window.atob(base64);
      const bytes = new Uint8Array(binaryString.length);
      for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i);
      }
      const file = new File([new Blob([bytes], { type: 'video/webm' })], 'recording.webm', { type: 'video/webm' });
      await processVideoFile(file);
    } catch (error) {
      console.error('Failed to load recording as source:', error);
    }
  };

      // Cleanup on unmount
    React.useEffect(() => {
      const currentFrameHolder = frameHolder.current;
      return () => {
        if (batchListenerRef.current) {
          batchListenerRef.current();
        }
        if (liveListenerRef.current) {
          liveListenerRef.current();
        }
        currentFrameHolder.stop();
      };
    }, []);

  return (
    <div className="min-h-screen bg-background">
      <AdBanner />
      <div className="max-w-7xl mx-auto p-6">
        <div className="grid lg:grid-cols-3 gap-6">
          {/* Main Video Area */}
          <div className="lg:col-span-2 space-y-4">
            {/* Video Preview */}
            <Card className="aspect-video bg-black/50 border-border relative overflow-hidden">
              {isLiveCamera ? (
                liveFrame ? (
                  <img
                    src={liveFrame}
                    alt="Live camera"
                    className="w-full h-full object-cover"
                  />
                ) : (
                  <div className="absolute inset-0 flex items-center justify-center bg-black">
                    <img src="/offline-placeholder.png" alt="Starting live camera" className="w-full h-full object-contain opacity-80" />
                    <div className="absolute bottom-12">
                      <p className="text-sm text-white/90 font-medium animate-pulse drop-shadow-lg">Starting live camera…</p>
                    </div>
                  </div>
                )
              ) : currentFrame ? (
                <img
                  src={`data:image/jpeg;base64,${btoa(String.fromCharCode(...currentFrame.data))}`}
                  alt="Video frame"
                  className="w-full h-full object-cover"
                />
              ) : videoInfo ? (
                <div className="absolute inset-0 flex items-center justify-center">
                  <div className="text-center space-y-4">
                    <div className="w-24 h-24 bg-primary/20 rounded-full flex items-center justify-center mx-auto">
                      <Video className="h-12 w-12 text-primary" />
                    </div>
                    <div>
                      <p className="text-lg font-medium">{videoMetadata.name}</p>
                      <p className="text-sm text-muted-foreground">
                        {isPlaying ? "Playing..." : "Ready to play"}
                      </p>
                    </div>
                  </div>
                </div>
              ) : (
                <div className="absolute inset-0 flex items-center justify-center">
                  <img src="/splash-screen.png" alt="CamLooper" className="absolute inset-0 w-full h-full object-cover opacity-20" />
                  <div className="text-center space-y-4 relative z-10">
                    <div className="w-24 h-24 bg-primary/20 backdrop-blur-sm rounded-full flex items-center justify-center mx-auto">
                      <Video className="h-12 w-12 text-primary" />
                    </div>
                    <div>
                      <p className="text-lg font-medium drop-shadow-md">{videoMetadata.name}</p>
                      <p className="text-sm text-muted-foreground drop-shadow-md">
                        {videoMetadata.dimensions && videoMetadata.fps 
                          ? `${videoMetadata.dimensions} • ${videoMetadata.fps} • ${videoMetadata.duration}`
                          : "Click Choose Files to load a video"
                        }
                      </p>
                    </div>
                  </div>
                </div>
              )}
              
              {/* Video Controls Overlay */}
              <div className="absolute bottom-0 left-0 right-0 bg-gradient-to-t from-black/80 to-transparent p-4">
                <div className="flex items-center justify-between">
                  <div className="flex items-center space-x-2">
                    <Button
                      variant="glassmorphism"
                      size="icon"
                      onClick={handlePlayPause}
                      disabled={!videoInfo || isLiveCamera}
                    >
                      {isPlaying ? (
                        <Pause className="h-4 w-4" />
                      ) : (
                        <Play className="h-4 w-4" />
                      )}
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleStop} disabled={!videoInfo || isLiveCamera}>
                      <Square className="h-4 w-4" />
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleRestart} disabled={!videoInfo || isLiveCamera}>
                      <RotateCcw className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
                
                {/* Progress Bar */}
                <div className="mt-2">
                  <div className="w-full bg-white/20 rounded-full h-2 transition-all">
                    <div 
                      className="bg-primary h-2 rounded-full transition-all"
                      style={{ 
                        width: streamStatus.duration > 0 ? `${Math.max(0, Math.min(100, (streamStatus.current_time / streamStatus.duration) * 100))}%` : '0%' 
                      }} 
                    />
                  </div>
                  <div className="flex justify-between text-xs text-white/80 mt-1">
                    <span>
                      {streamStatus.duration > 0 
                        ? `${Math.floor(streamStatus.current_time / 60)}:${Math.floor(streamStatus.current_time % 60).toString().padStart(2, '0')}`
                        : '0:00'
                      }
                    </span>
                    <span>
                      {streamStatus.duration > 0 
                        ? `${Math.floor(streamStatus.duration / 60)}:${Math.floor(streamStatus.duration % 60).toString().padStart(2, '0')}`
                        : '0:00'
                      }
                    </span>
                  </div>
                </div>
              </div>

              {/* Loop Indicator */}
              {isPlaying && (
                <div className="absolute top-4 right-4">
                  <Badge variant="secondary" className="bg-primary/20 text-primary border-primary/40">
                    <RotateCcw className="h-3 w-3 mr-1" />
                    Loop {streamStatus.current_loop + 1}/{streamStatus.loop_count === 10 ? '∞' : streamStatus.loop_count}
                  </Badge>
                </div>
              )}
              
              {/* Loop Complete Indicator */}
              {isLoopingComplete && (
                <div className="absolute top-4 right-4">
                  <Badge variant="secondary" className="bg-green-500/20 text-green-400 border-green-500/40">
                    <RotateCcw className="h-3 w-3 mr-1" />
                    Looping Complete!
                  </Badge>
                </div>
              )}

              {/* Buffer Health Indicator */}
              {isPlaying && (
                <div className="absolute top-16 right-4">
                  <div className="bg-black/50 rounded-lg p-2 text-xs text-white">
                    <div className="flex items-center space-x-2">
                      <div className="w-2 h-2 rounded-full bg-green-500"></div>
                      <span>Buffer: {Math.round(streamStatus.buffer_health * 100)}%</span>
                    </div>
                    <div className="flex items-center space-x-2 mt-1">
                      <div className="w-2 h-2 rounded-full bg-blue-500"></div>
                      <span>FPS: {streamStatus.actual_fps.toFixed(1)}</span>
                    </div>
                    {streamStatus.frames_dropped > 0 && (
                      <div className="flex items-center space-x-2 mt-1">
                        <div className="w-2 h-2 rounded-full bg-yellow-500"></div>
                        <span>Dropped: {streamStatus.frames_dropped}</span>
                      </div>
                    )}
                  </div>
                </div>
              )}
              
              {/* Performance Metrics Toggle */}
              {videoInfo && (
                <div className="absolute bottom-16 right-4">
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => setShowPerformanceMetrics(!showPerformanceMetrics)}
                    className="bg-black/50 border-white/20 text-white hover:bg-black/70"
                  >
                    <Settings className="h-3 w-3 mr-1" />
                    Metrics
                  </Button>
                </div>
              )}

              {/* Upload Progress */}
              {isUploading && (
                <div className="absolute top-4 left-4">
                  <Badge variant="secondary" className="bg-blue-500/20 text-blue-400 border-blue-500/40">
                    Uploading... {uploadProgress}%
                  </Badge>
                </div>
              )}

              {/* Live Camera Indicator */}
              {isLiveCamera && (
                <div className="absolute top-4 left-4 flex items-center space-x-2">
                  <Badge variant="secondary" className="bg-red-500/20 text-red-400 border-red-500/40">
                    <Radio className="h-3 w-3 mr-1" />
                    LIVE
                  </Badge>
                  {isLiveRecording && (
                    <Badge variant="secondary" className="bg-red-600/30 text-red-300 border-red-500/50">
                      <Circle className="h-2 w-2 mr-1 fill-current animate-pulse" />
                      REC
                    </Badge>
                  )}
                </div>
              )}
            </Card>

            {/* Performance Metrics Panel */}
            {showPerformanceMetrics && (
              <Card className="mt-4 p-4 bg-black/5 border-gray-700">
                <div className="space-y-3">
                  <div className="flex items-center justify-between">
                    <h3 className="font-semibold text-sm">Performance Metrics</h3>
                    <Button
                      variant="ghost"
                      size="sm"
                      onClick={() => setShowPerformanceMetrics(false)}
                    >
                      ×
                    </Button>
                  </div>
                  
                  <div className="grid grid-cols-2 gap-4 text-xs">
                    <div>
                      <span className="text-muted-foreground">Frames Processed:</span>
                      <span className="ml-2 font-mono">{performanceMetrics.frames_processed}</span>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Frames Dropped:</span>
                      <span className="ml-2 font-mono">{performanceMetrics.frames_dropped}</span>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Encode Time:</span>
                      <span className="ml-2 font-mono">{performanceMetrics.average_encode_time.toFixed(2)}ms</span>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Current Quality:</span>
                      <span className="ml-2 font-mono">{performanceMetrics.current_quality}%</span>
                    </div>
                    <div>
                      <span className="text-muted-foreground">Status:</span>
                      <span className="ml-2 font-mono">
                        {isPlaying ? 'Playing' : 'Stopped'}
                      </span>
                    </div>
                    <div>
                      <span className="text-muted-foreground">FPS:</span>
                      <span className="ml-2 font-mono">{streamStatus.actual_fps.toFixed(1)}/{streamStatus.target_fps.toFixed(0)}</span>
                    </div>
                  </div>
                  
                  {/* Playback Status Bar */}
                  <div className="mt-3">
                    <div className="flex justify-between text-xs mb-1">
                      <span>Playback Status</span>
                      <span>
                        {isPlaying ? 'Playing' : 'Stopped'}
                      </span>
                    </div>
                    <div className="w-full bg-gray-700 rounded-full h-2">
                      <div 
                        className={`h-2 rounded-full transition-all ${
                          isPlaying ? 'bg-green-500' : 'bg-gray-500'
                        }`}
                        style={{ 
                          width: isPlaying ? '100%' : '0%'
                        }}
                      />
                    </div>
                  </div>
                </div>
              </Card>
            )}

            {/* Hidden canvas for frame processing */}
            <canvas
              ref={canvasRef}
              style={{ display: 'none' }}
              width={1920}
              height={1080}
            />

            {/* Upload Area */}
            <Card className="border-dashed border-2 border-border hover:border-primary/40 transition-colors">
              <div 
                className="p-8 text-center"
                onDrop={handleDrop}
                onDragOver={handleDragOver}
                onDragEnter={handleDragOver}
              >
                <Upload className="h-8 w-8 text-muted-foreground mx-auto mb-2" />
                <p className="text-lg font-medium mb-1">Choose Video File</p>
                <p className="text-sm text-muted-foreground mb-4">
                  Supports MP4, MOV, AVI, WebM, MKV, FLV, WMV, M4V
                </p>
                {uploadError && (
                  <div className="mb-4 p-3 bg-red-500/20 text-red-400 border border-red-500/40 rounded text-sm">
                    <strong>Upload Error:</strong> {uploadError}
                  </div>
                )}
                <Button variant="outline" onClick={triggerFileInput} disabled={isUploading}>
                  <Upload className="h-4 w-4 mr-2" />
                  {isUploading ? 'Uploading...' : 'Choose Files'}
                </Button>
                <input
                  id="video-file-input"
                  type="file"
                  accept="video/*"
                  onChange={handleFileSelect}
                  className="hidden"
                />
              </div>
            </Card>
          </div>

          {/* Control Panel */}
          <div className="space-y-6">
            {/* Virtual Camera Status */}
            <Card className="p-4">
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <h3 className="font-semibold flex items-center">
                    <Monitor className="h-4 w-4 mr-2" />
                    Virtual Camera
                  </h3>
                  <Badge variant={virtualCameraStatus.is_active ? "default" : "secondary"} className={virtualCameraStatus.is_active ? "bg-green-500/20 text-green-400 border-green-500/40" : ""}>
                    {virtualCameraStatus.is_active ? "Active" : "Inactive"}
                  </Badge>
                </div>
                
                <div className="space-y-3">
                  <div className="flex items-center justify-between">
                    <span className="text-sm">Enable Virtual Camera</span>
                    <Switch 
                      checked={isVirtualCamActive}
                      onCheckedChange={handleVirtualCameraToggle}
                      disabled={isVirtualCamLoading}
                    />
                  </div>
                  
                  <div className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">
                    Status: {virtualCameraStatus.is_active ? 'Ready for apps to use' : 'Disabled'}
                    {virtualCameraStatus.is_active && (
                      <div className="mt-1">
                        <div>Resolution: {virtualCameraStatus.resolution}</div>
                        <div>FPS: {virtualCameraStatus.fps}</div>
                        <div>Frames: {virtualCameraStatus.frame_count}</div>
                      </div>
                    )}
                  </div>
                </div>
              </div>
            </Card>

            {/* Live Camera */}
            <Card className="p-4">
              <div className="space-y-4">
                <div className="flex items-center justify-between">
                  <h3 className="font-semibold flex items-center">
                    <Radio className="h-4 w-4 mr-2" />
                    Live Camera
                  </h3>
                  <Badge variant={isLiveCamera ? "default" : "secondary"} className={isLiveCamera ? "bg-red-500/20 text-red-400 border-red-500/40" : ""}>
                    {isLiveCamera ? "Live" : "Off"}
                  </Badge>
                </div>

                <div className="space-y-3">
                  {liveDevices.length > 0 ? (
                    <select
                      className="w-full p-2 bg-gray-900 border border-gray-800 rounded-md text-sm text-white focus:outline-none focus:ring-2 focus:ring-primary/50"
                      value={selectedLiveDevice}
                      onChange={(e) => setSelectedLiveDevice(e.target.value)}
                      disabled={isLiveCamera || isLiveLoading}
                    >
                      {liveDevices.map(d => {
                        const path = d.split(':')[0];
                        return <option key={path} value={path}>{d}</option>;
                      })}
                    </select>
                  ) : (
                    <p className="text-xs text-muted-foreground">No camera devices found.</p>
                  )}

                  <div className="flex items-center justify-between">
                    <span className="text-sm">Go Live (Camera → Zoom)</span>
                    <Switch
                      checked={isLiveCamera}
                      onCheckedChange={handleLiveCameraToggle}
                      disabled={isLiveLoading || liveDevices.length === 0}
                    />
                  </div>

                  {isLiveCamera && (
                    <Button
                      variant={isLiveRecording ? "destructive" : "outline"}
                      size="sm"
                      className="w-full"
                      onClick={handleLiveRecordToggle}
                    >
                      {isLiveRecording ? (
                        <>
                          <Square className="h-4 w-4 mr-2" />
                          Stop Recording
                        </>
                      ) : (
                        <>
                          <Circle className="h-4 w-4 mr-2 fill-current" />
                          Record to File
                        </>
                      )}
                    </Button>
                  )}

                  {isLiveCamera && (
                    <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">
                      {isLiveRecording
                        ? "Recording to a file while still streaming to Zoom."
                        : "Streaming live camera to the virtual camera. Recording also keeps streaming to Zoom."}
                    </p>
                  )}

                  {lastRecordingPath && !isLiveRecording && (
                    <div className="text-xs text-muted-foreground bg-muted/50 p-2 rounded space-y-2">
                      <div className="break-all">Saved: {lastRecordingPath}</div>
                      <Button variant="ghost" size="sm" className="w-full" onClick={useLiveRecordingAsSource}>
                        Use as loop source
                      </Button>
                    </div>
                  )}
                </div>
              </div>
            </Card>

            {/* Loop Settings */}
            <Card className="p-4">
              <div className="space-y-4">
                <h3 className="font-semibold flex items-center">
                  <RotateCcw className="h-4 w-4 mr-2" />
                  Loop Settings
                </h3>
                
                <div className="space-y-4">
                  <div>
                    <div className="flex items-center justify-between mb-2">
                      <span className="text-sm">Loop Count</span>
                      <span className="text-sm text-primary">{loopCount[0] === 10 ? '∞' : loopCount[0]}</span>
                    </div>
                    <Slider
                      value={loopCount}
                      onValueChange={setLoopCount}
                      max={10}
                      min={1}
                      step={1}
                      className="w-full"
                    />
                    <div className="flex justify-between text-xs text-muted-foreground mt-1">
                      <span>1</span>
                      <span>∞</span>
                    </div>
                  </div>

                  <div className="space-y-2">
                    <div className="flex items-center justify-between">
                      <span className="text-sm">Auto Start</span>
                      <Switch checked={autoStart} onCheckedChange={setAutoStart} />
                    </div>
                  </div>
                </div>
              </div>
            </Card>

            {/* Video Info */}
            {videoInfo && (
              <Card className="p-4">
                <div className="space-y-4">
                  <h3 className="font-semibold">Video Information</h3>
                  
                  <div className="space-y-2 text-sm">
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">Name:</span>
                      <span>{videoInfo.filename}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">Duration:</span>
                      <span>{Math.floor(videoInfo.duration / 60)}:{Math.floor(videoInfo.duration % 60).toString().padStart(2, '0')}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">Resolution:</span>
                      <span>{videoInfo.width}x{videoInfo.height}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">FPS:</span>
                      <span>{Math.round(videoInfo.fps)}</span>
                    </div>
                    <div className="flex justify-between">
                      <span className="text-muted-foreground">Format:</span>
                      <span>{videoInfo.format}</span>
                    </div>
                  </div>
                </div>
              </Card>
            )}



            {/* Quick Actions */}
            <div className="space-y-2">
              <VideoRecorder onRecordingComplete={processVideoFile} />
            </div>
          </div>
        </div>

        {/* Debug Logs */}
        <div className="mt-6">
          <Card className="p-4">
            <div className="space-y-4">
              <div className="flex items-center justify-between">
                <h3 className="font-semibold flex items-center">
                  <FileText className="h-4 w-4 mr-2" />
                  <span className="text-sm">Debug Logs</span>
                  <Badge variant="secondary" className="ml-2">
                    {logs.length}
                  </Badge>
                </h3>
                <div className="flex items-center space-x-2">
                  <Button variant="ghost" size="sm" onClick={exportLogs} disabled={logs.length === 0}>
                    <Download className="h-3 w-3 mr-1" />
                    Export
                  </Button>
                  <Button variant="ghost" size="sm" onClick={clearLogs} disabled={logs.length === 0}>
                    <Trash2 className="h-3 w-3 mr-1" />
                    Clear
                  </Button>
                  <Button 
                    variant="ghost" 
                    size="sm" 
                    onClick={() => setShowLogs(!showLogs)}
                  >
                    {showLogs ? <EyeOff className="h-3 w-3 mr-1" /> : <Eye className="h-3 w-3 mr-1" />}
                    {showLogs ? 'Hide' : 'Show'}
                  </Button>
                </div>
              </div>
              
              {showLogs && (
                <div className="relative">
                  <textarea
                    ref={logTextareaRef}
                    value={logs.join('\n')}
                    readOnly
                    className="w-full h-40 p-3 text-xs font-mono bg-gray-950 text-green-400 rounded border border-gray-700 resize-none focus:outline-none focus:ring-2 focus:ring-primary/50 overflow-y-auto"
                    placeholder="Debug logs will appear here..."
                    style={{
                      fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Consolas, "Liberation Mono", Menlo, monospace',
                      lineHeight: '1.4'
                    }}
                  />
                  <div className="absolute bottom-2 right-2 text-xs text-gray-500">
                    {logs.length} entries
                  </div>
                </div>
              )}
              
              {!showLogs && logs.length > 0 && (
                <div className="text-sm text-muted-foreground">
                  Latest: {logs[logs.length - 1]?.substring(0, 100)}...
                </div>
              )}
            </div>
          </Card>
        </div>
      </div>
    </div>
  );
}

const App = () => (
  <QueryClientProvider client={queryClient}>
    <TooltipProvider>
      <Toaster />
      <Sonner />
      <CamLooper />
    </TooltipProvider>
  </QueryClientProvider>
);

export default App;
