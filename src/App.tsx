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
  RotateCcw
} from "lucide-react";
import React, { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

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
  data: string; // Base64 encoded JPEG
  timestamp: number;
  width: number;
  height: number;
}

interface StreamStatus {
  is_playing: boolean;
  current_time: number;
  duration: number;
  loop_count: number;
  current_loop: number;
}

function CamLooper() {
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVirtualCamActive, setIsVirtualCamActive] = useState(false);
  const [loopCount, setLoopCount] = useState([5]);
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
    loop_count: 5,
    current_loop: 0
  });
  const [isBuffering, setIsBuffering] = useState(false);
  const [isUploading, setIsUploading] = useState(false);
  const [uploadProgress, setUploadProgress] = useState(0);
  const [autoStart, setAutoStart] = useState(true);
  const [isLoopingComplete, setIsLoopingComplete] = useState(false);
  const [virtualCameraStatus, setVirtualCameraStatus] = useState<VirtualCameraStatus>({
    is_active: false,
    camera_name: "CamLooper Virtual Camera",
    resolution: "1920x1080",
    fps: 30,
    frame_count: 0
  });
  const [isVirtualCamLoading, setIsVirtualCamLoading] = useState(false);
  const canvasRef = React.useRef<HTMLCanvasElement>(null);
  const frameIntervalRef = React.useRef<number | null>(null);

  // Helper function to convert file to base64
  const fileToBase64 = (file: File): Promise<string> => {
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
  };

  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file && file.type.startsWith('video/')) {
      setSelectedVideo(file);
      setIsUploading(true);
      setUploadProgress(0);
      
      try {
        // Convert file to base64
        const base64Data = await fileToBase64(file);
        
        // Upload and load video in backend
        const videoInfo = await invoke<VideoInfo>('upload_and_load_video', {
          filename: file.name,
          fileData: base64Data
        });
        
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
        
        // Reset state
        setIsPlaying(false);
        setIsBuffering(false);
        setIsLoopingComplete(false);
        
        // Auto start if enabled
        if (autoStart) {
          await handlePlayPause();
        }
        
      } catch (error) {
        console.error('Error uploading video:', error);
        // Reset on error
        setSelectedVideo(null);
        setVideoInfo(null);
      } finally {
        setIsUploading(false);
        setUploadProgress(100);
      }
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
    
    if (file && file.type.startsWith('video/')) {
      // Simulate file input change event
      const fakeEvent = {
        target: { files: [file] }
      } as unknown as React.ChangeEvent<HTMLInputElement>;
      handleFileSelect(fakeEvent);
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
        if (frameIntervalRef.current) {
          clearInterval(frameIntervalRef.current);
          frameIntervalRef.current = null;
        }
      } else {
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
      if (frameIntervalRef.current) {
        clearInterval(frameIntervalRef.current);
        frameIntervalRef.current = null;
      }
      setCurrentFrame(null);
    } catch (error) {
      console.error('Error stopping video:', error);
    }
  };

  const handleRestart = async () => {
    if (!videoInfo) return;
    
    try {
      await invoke('stop_video_stream');
      await invoke('start_video_stream');
      setIsPlaying(true);
      await startFrameReceiving();
    } catch (error) {
      console.error('Error restarting video:', error);
    }
  };

  const startFrameReceiving = async () => {
    // Stop any existing polling
    if (frameIntervalRef.current) {
      clearInterval(frameIntervalRef.current);
      frameIntervalRef.current = null;
    }
    
    // Listen for push-based video frames
    try {
      await listen<VideoFrame>('video-frame', (event) => {
        const frame = event.payload;
        setCurrentFrame(frame);
        
        // Send frame to virtual camera if active
        if (isVirtualCamActive) {
          invoke('send_frame_to_virtual_camera', { frameData: frame.data }).catch(
            error => console.error('Error sending frame to virtual camera:', error)
          );
        }
      });
    } catch (error) {
      console.error('Error setting up video frame listener:', error);
    }
  };

  // Update stream status periodically
  useEffect(() => {
    if (!videoInfo) return;

    const statusInterval = setInterval(async () => {
      try {
        const status = await invoke<StreamStatus>('get_video_stream_status');
        setStreamStatus(status);
        
        if (!status.is_playing && isPlaying) {
          setIsPlaying(false);
          if (frameIntervalRef.current) {
            clearInterval(frameIntervalRef.current);
            frameIntervalRef.current = null;
          }
          
          // Check if looping is complete
          if (status.current_loop >= status.loop_count && status.loop_count !== 10) {
            setIsLoopingComplete(true);
            setTimeout(() => setIsLoopingComplete(false), 3000);
          }
        }
      } catch (error) {
        console.error('Error getting stream status:', error);
      }
    }, 200); // Reduce polling frequency from 100ms to 200ms

    return () => clearInterval(statusInterval);
  }, [videoInfo, isPlaying]);

  // Update loop settings when changed
  useEffect(() => {
    if (videoInfo) {
      invoke('set_video_loop_settings', {
        loopCount: loopCount[0],
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

  // Cleanup on unmount
  React.useEffect(() => {
    return () => {
      if (frameIntervalRef.current) {
        clearInterval(frameIntervalRef.current);
      }
    };
  }, []);

  return (
    <div className="min-h-screen bg-background p-6">
      <div className="max-w-7xl mx-auto">
        <div className="grid lg:grid-cols-3 gap-6">
          {/* Main Video Area */}
          <div className="lg:col-span-2 space-y-4">
            {/* Video Preview */}
            <Card className="aspect-video bg-black/50 border-border relative overflow-hidden">
              {currentFrame ? (
                <img
                  src={`data:image/jpeg;base64,${currentFrame.data}`}
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
                        {isBuffering ? "Loading..." : "Ready to play"}
                      </p>
                    </div>
                  </div>
                </div>
              ) : (
                <div className="absolute inset-0 flex items-center justify-center">
                  <div className="text-center space-y-4">
                    <div className="w-24 h-24 bg-primary/20 rounded-full flex items-center justify-center mx-auto">
                      <Video className="h-12 w-12 text-primary" />
                    </div>
                    <div>
                      <p className="text-lg font-medium">{videoMetadata.name}</p>
                      <p className="text-sm text-muted-foreground">
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
                      disabled={isBuffering || !videoInfo}
                    >
                      {isBuffering ? (
                        <div className="w-4 h-4 border-2 border-white border-t-transparent rounded-full animate-spin" />
                      ) : isPlaying ? (
                        <Pause className="h-4 w-4" />
                      ) : (
                        <Play className="h-4 w-4" />
                      )}
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleStop} disabled={!videoInfo}>
                      <Square className="h-4 w-4" />
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleRestart} disabled={!videoInfo}>
                      <RotateCcw className="h-4 w-4" />
                    </Button>
                  </div>
                </div>
                
                {/* Progress Bar */}
                <div className="mt-2">
                  <div className={`w-full bg-white/20 rounded-full h-2 transition-all relative ${isBuffering ? 'opacity-50' : ''}`}>
                    <div 
                      className={`bg-primary h-2 rounded-full transition-all ${isBuffering ? 'animate-pulse' : ''}`}
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

              {/* Upload Progress */}
              {isUploading && (
                <div className="absolute top-4 left-4">
                  <Badge variant="secondary" className="bg-blue-500/20 text-blue-400 border-blue-500/40">
                    Uploading... {uploadProgress}%
                  </Badge>
                </div>
              )}
            </Card>

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

            {/* App Compatibility */}
            <Card className="p-4">
              <div className="space-y-4">
                <h3 className="font-semibold">Compatible Apps</h3>
                
                <div className="space-y-2">
                  {[
                    { name: "Zoom", status: "detected" },
                    { name: "Discord", status: "ready" },
                    { name: "OBS Studio", status: "ready" },
                    { name: "Microsoft Teams", status: "ready" }
                  ].map((app, index) => (
                    <div key={index} className="flex items-center justify-between p-2 bg-muted/50 rounded">
                      <span className="text-sm">{app.name}</span>
                      <Badge variant="secondary" className={
                        app.status === "detected" 
                          ? "bg-green-500/20 text-green-400 border-green-500/40" 
                          : "bg-muted-foreground/20"
                      }>
                        {app.status === "detected" ? "Detected" : "Ready"}
                      </Badge>
                    </div>
                  ))}
                </div>
              </div>
            </Card>

            {/* Quick Actions */}
            <div className="space-y-2">
              <Button variant="outline" size="sm" className="w-full">
                <Settings className="h-4 w-4 mr-2" />
                Advanced Settings
              </Button>
              <Button variant="outline" size="sm" className="w-full">
                <Video className="h-4 w-4 mr-2" />
                Record New Video
              </Button>
            </div>
          </div>
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
