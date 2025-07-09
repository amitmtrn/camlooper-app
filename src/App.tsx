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
import React, { useState } from "react";

const queryClient = new QueryClient();

function CamLooper() {
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVirtualCamActive, setIsVirtualCamActive] = useState(true);
  const [loopCount, setLoopCount] = useState([5]);
  const [selectedVideo, setSelectedVideo] = useState<File | null>(null);
  const [videoUrl, setVideoUrl] = useState<string | null>(null);
  const [videoMetadata, setVideoMetadata] = useState({
    name: "No Video Selected",
    duration: "0:00",
    dimensions: "",
    fps: ""
  });
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [isBuffering, setIsBuffering] = useState(false);
  const [isMuted, setIsMuted] = useState(true);
  const [autoStart, setAutoStart] = useState(true);
  const videoRef = React.useRef<HTMLVideoElement>(null);

  const handleFileSelect = (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file && file.type.startsWith('video/')) {
      setSelectedVideo(file);
      
      // Create new video URL
      const url = URL.createObjectURL(file);
      setVideoUrl(url);
      
      // Reset playback state
      setIsPlaying(false);
      setCurrentTime(0);
      setIsBuffering(false);
      setIsMuted(true);
      // Note: Keep autoStart setting as user preference
      
      // Create a separate video element to extract metadata
      const metadataVideo = document.createElement('video');
      metadataVideo.preload = 'metadata';
      
      // Create a separate blob URL for metadata extraction
      const metadataUrl = URL.createObjectURL(file);
      
      metadataVideo.onloadedmetadata = () => {
        const duration = metadataVideo.duration;
        const minutes = Math.floor(duration / 60);
        const seconds = Math.floor(duration % 60);
        const formattedDuration = `${minutes}:${seconds.toString().padStart(2, '0')}`;
        
        setVideoMetadata({
          name: file.name,
          duration: formattedDuration,
          dimensions: `${metadataVideo.videoWidth}x${metadataVideo.videoHeight}`,
          fps: "30fps" // This is harder to detect, so we'll keep a default
        });
        
        setDuration(metadataVideo.duration);
        setCurrentTime(0);
        // Clean up the metadata video URL
        URL.revokeObjectURL(metadataUrl);
      };
      
      metadataVideo.onerror = () => {
        console.error('Error loading video metadata for file:', file.name);
        if (metadataVideo.error) {
          console.error('Metadata error code:', metadataVideo.error.code);
          console.error('Metadata error message:', metadataVideo.error.message);
        }
        // Clean up the metadata video URL
        URL.revokeObjectURL(metadataUrl);
      };
      
      metadataVideo.src = metadataUrl;
    } else {
      console.error('Invalid file selected:', file?.name, 'Type:', file?.type);
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
    if (!videoRef.current || !videoUrl) {
      return;
    }

    try {
      if (isPlaying) {
        videoRef.current.pause();
      } else {
        await videoRef.current.play();
      }
    } catch (error) {
      console.error('Error with video playback:', error);
      setIsPlaying(false);
    }
  };

  const handleStop = () => {
    if (videoRef.current && videoUrl) {
      videoRef.current.pause();
      videoRef.current.currentTime = 0;
    }
    setIsPlaying(false);
  };

  const handleRestart = () => {
    if (videoRef.current && videoUrl) {
      videoRef.current.currentTime = 0;
      if (isPlaying) {
        videoRef.current.play();
      }
    }
  };

  const handleSeek = (event: React.MouseEvent<HTMLDivElement>) => {
    if (videoRef.current && duration > 0) {
      const rect = event.currentTarget.getBoundingClientRect();
      const clickX = event.clientX - rect.left;
      const seekTime = (clickX / rect.width) * duration;
      videoRef.current.currentTime = seekTime;
      setCurrentTime(seekTime); // Update state immediately for UI responsiveness
    }
  };

  // Keep video muted
  React.useEffect(() => {
    if (videoRef.current) {
      videoRef.current.volume = 0;
      videoRef.current.muted = true;
    }
  }, [videoUrl]);

  // Handle autoStart toggle - start video when enabled
  React.useEffect(() => {
    if (autoStart && videoRef.current && videoUrl && !isPlaying) {
      const video = videoRef.current;
      if (video.readyState >= 2) {
        video.play().catch((error) => {
          console.log('Auto-play prevented:', error);
        });
      }
    }
  }, [autoStart, videoUrl, isPlaying]);

  // Add event listeners to video element
  React.useEffect(() => {
    const video = videoRef.current;
    if (!video) return;

    const handleTimeUpdate = () => {
      setCurrentTime(video.currentTime);
    };

    const handleLoadedMetadata = () => {
      setDuration(video.duration);
      setCurrentTime(0);
      setIsBuffering(false);
      
      // Auto start video if enabled
      if (autoStart) {
        video.play().catch((error) => {
          console.log('Auto-play prevented:', error);
        });
      }
    };

    const handleEnded = () => {
      setIsPlaying(false);
    };

    const handlePlay = () => {
      setIsPlaying(true);
      setIsBuffering(false);
    };

    const handlePause = () => {
      setIsPlaying(false);
    };

    const handleWaiting = () => {
      setIsBuffering(true);
    };

    const handleCanPlay = () => {
      setIsBuffering(false);
    };

    const handleStalled = () => {
      setIsBuffering(true);
      console.log('Video playback stalled');
    };

    const handleError = (e: Event) => {
      const videoElement = e.target as HTMLVideoElement;
      if (videoElement && videoElement.error) {
        console.error('Video error:', videoElement.error.message, 'Code:', videoElement.error.code);
      }
      setIsPlaying(false);
      setIsBuffering(false);
    };

    const handleSeeked = () => {
      setCurrentTime(video.currentTime);
    };

    const handleDurationChange = () => {
      setDuration(video.duration);
    };

    video.addEventListener('timeupdate', handleTimeUpdate);
    video.addEventListener('loadedmetadata', handleLoadedMetadata);
    video.addEventListener('ended', handleEnded);
    video.addEventListener('play', handlePlay);
    video.addEventListener('pause', handlePause);
    video.addEventListener('waiting', handleWaiting);
    video.addEventListener('canplay', handleCanPlay);
    video.addEventListener('stalled', handleStalled);
    video.addEventListener('error', handleError);
    video.addEventListener('seeked', handleSeeked);
    video.addEventListener('durationchange', handleDurationChange);

    return () => {
      video.removeEventListener('timeupdate', handleTimeUpdate);
      video.removeEventListener('loadedmetadata', handleLoadedMetadata);
      video.removeEventListener('ended', handleEnded);
      video.removeEventListener('play', handlePlay);
      video.removeEventListener('pause', handlePause);
      video.removeEventListener('waiting', handleWaiting);
      video.removeEventListener('canplay', handleCanPlay);
      video.removeEventListener('stalled', handleStalled);
      video.removeEventListener('error', handleError);
      video.removeEventListener('seeked', handleSeeked);
      video.removeEventListener('durationchange', handleDurationChange);
    };
  }, [videoUrl]);

  // Keep track of previous URL for cleanup
  const previousUrlRef = React.useRef<string | null>(null);

  React.useEffect(() => {
    // Clean up previous URL when a new one is set
    if (previousUrlRef.current && previousUrlRef.current !== videoUrl) {
      URL.revokeObjectURL(previousUrlRef.current);
    }
    previousUrlRef.current = videoUrl;

    // Cleanup on unmount
    return () => {
      if (videoUrl) {
        URL.revokeObjectURL(videoUrl);
      }
    };
  }, [videoUrl]);

  return (
    <div className="min-h-screen bg-background p-6">
      <div className="max-w-7xl mx-auto">
        <div className="grid lg:grid-cols-3 gap-6">
          {/* Main Video Area */}
          <div className="lg:col-span-2 space-y-4">
            {/* Video Preview */}
            <Card className="aspect-video bg-black/50 border-border relative overflow-hidden">
              {videoUrl ? (
                <video
                  ref={videoRef}
                  className="w-full h-full object-cover"
                  src={videoUrl}
                  controls={false}
                  muted={true}
                  loop
                  preload="auto"
                  playsInline

                />
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
                      disabled={isBuffering}
                    >
                      {isBuffering ? (
                        <div className="w-4 h-4 border-2 border-white border-t-transparent rounded-full animate-spin" />
                      ) : isPlaying ? (
                        <Pause className="h-4 w-4" />
                      ) : (
                        <Play className="h-4 w-4" />
                      )}
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleStop}>
                      <Square className="h-4 w-4" />
                    </Button>
                    <Button variant="glassmorphism" size="icon" onClick={handleRestart}>
                      <RotateCcw className="h-4 w-4" />
                    </Button>
                  </div>
                  
                  {/* Debug button */}
                  <Button 
                    variant="glassmorphism" 
                    size="sm" 
                    onClick={() => {
                      console.log('=== DEBUG INFO ===');
                      console.log('Video URL:', videoUrl);
                      console.log('Selected video file:', selectedVideo);
                      console.log('Video metadata:', videoMetadata);
                      console.log('Is playing:', isPlaying);
                      console.log('Auto start:', autoStart);
                      if (videoRef.current) {
                        console.log('Video element:', videoRef.current);
                        console.log('Video src:', videoRef.current.src);
                        console.log('Video readyState:', videoRef.current.readyState);
                        console.log('Video paused:', videoRef.current.paused);
                        console.log('Video currentTime:', videoRef.current.currentTime);
                        console.log('Video duration:', videoRef.current.duration);
                        console.log('Video muted:', videoRef.current.muted);
                        console.log('Video volume:', videoRef.current.volume);
                        if (videoRef.current.error) {
                          console.log('Video error:', videoRef.current.error);
                          console.log('Error code:', videoRef.current.error.code);
                          console.log('Error message:', videoRef.current.error.message);
                        }
                      }
                      console.log('==================');
                    }}
                  >
                    Debug
                  </Button>

                </div>
                
                {/* Progress Bar */}
                <div className="mt-2">
                  <div 
                    className="w-full bg-white/20 rounded-full h-2 cursor-pointer hover:h-3 transition-all relative"
                    onClick={handleSeek}
                    title={`Click to seek - ${duration > 0 ? `${Math.floor(currentTime / 60)}:${Math.floor(currentTime % 60).toString().padStart(2, '0')} / ${Math.floor(duration / 60)}:${Math.floor(duration % 60).toString().padStart(2, '0')}` : 'No video loaded'}`}
                  >
                    <div 
                      className="bg-primary h-2 rounded-full hover:h-3 transition-all" 
                      style={{ 
                        width: duration > 0 ? `${Math.max(0, Math.min(100, (currentTime / duration) * 100))}%` : '0%' 
                      }} 
                    />
                    {/* Seek handle */}
                    {duration > 0 && (
                      <div 
                        className="absolute top-0 w-3 h-3 bg-white rounded-full shadow-md transform -translate-y-0.5 -translate-x-1.5 opacity-0 hover:opacity-100 transition-opacity"
                        style={{ 
                          left: `${Math.max(0, Math.min(100, (currentTime / duration) * 100))}%` 
                        }}
                      />
                    )}
                  </div>
                  <div className="flex justify-between text-xs text-white/80 mt-1">
                    <span>
                      {duration > 0 
                        ? `${Math.floor(currentTime / 60)}:${Math.floor(currentTime % 60).toString().padStart(2, '0')}`
                        : '0:00'
                      }
                    </span>
                    <span>
                      {duration > 0 
                        ? `${Math.floor(duration / 60)}:${Math.floor(duration % 60).toString().padStart(2, '0')}`
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
                    Loop Active
                  </Badge>
                </div>
              )}
            </Card>

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
                  Supports MP4, MOV, AVI, WebM
                </p>
                <Button variant="outline" onClick={triggerFileInput}>
                  <Upload className="h-4 w-4 mr-2" />
                  Choose Files
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
                  <Badge variant={isVirtualCamActive ? "default" : "secondary"} className={isVirtualCamActive ? "bg-green-500/20 text-green-400 border-green-500/40" : ""}>
                    {isVirtualCamActive ? "Active" : "Inactive"}
                  </Badge>
                </div>
                
                <div className="space-y-3">
                  <div className="flex items-center justify-between">
                    <span className="text-sm">Enable Virtual Camera</span>
                    <Switch 
                      checked={isVirtualCamActive}
                      onCheckedChange={setIsVirtualCamActive}
                    />
                  </div>
                  
                  <div className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">
                    Status: {isVirtualCamActive ? 'Ready for apps to use' : 'Disabled'}
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
