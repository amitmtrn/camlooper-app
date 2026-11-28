import { Toaster } from "@/components/ui/toaster";
import { Toaster as Sonner, toast } from "@/components/ui/sonner";
import { TooltipProvider } from "@/components/ui/tooltip";
import { Button } from "@/components/ui/button";
import { Card } from "@/components/ui/card";
import { Badge } from "@/components/ui/badge";
import { Slider } from "@/components/ui/slider";
import { Switch } from "@/components/ui/switch";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
  SelectSuppressedContext,
} from "@/components/ui/select";
import {
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
  Circle,
  Camera,
  ArrowRight,
  ChevronDown,
  Loader2,
  ShieldCheck,
  Globe
} from "lucide-react";
import React, { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useTranslation, Trans } from "react-i18next";
import { AdBanner } from "./components/AdBanner";
import { useWelcomePopup } from "./hooks/use-welcome-popup";
import { useWindowShown } from "./hooks/use-window-shown";
import { useRemote, useRemoteCommands, type OpenMeetingCommand } from "./hooks/use-remote";
import { RemoteCard, type RemoteCardHandle } from "./components/RemoteCard";
import { usePhoneStream, cameraBusy, isolate, requestKey, type LoadedVideo } from "./hooks/use-phone-stream";
import { PhoneStreamBanner, PhoneStreamDialog } from "./components/PhoneStream";
import { useMeeting, useMeetingStatus } from "@/hooks/use-meeting";
import { Stepper } from "./components/flow/Stepper";
import { SUPPORTED_LANGUAGES, loadLanguage } from "./i18n";
import { cn } from "@/lib/utils";
import { loadNaturalMotion, saveNaturalMotion } from "@/lib/playback-settings";


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
  /// Populated once when the frame arrives; see frameToDataUrl.
  url?: string;
}

/**
 * Output resolutions offered in Advanced → Virtual Camera.
 *
 * The source render and the camera output are always the same size, so nothing is
 * rescaled in between. They used to disagree — the video was rendered 640px wide and the
 * camera advertised 1920x1080 — so every frame was upscaled from 360p to 1080p, which
 * added no detail and cost more per frame than everything else combined.
 *
 * 720p is the default: it is what conferencing apps request, and it halves the per-frame
 * copy compared to 1080p.
 */
const RESOLUTIONS = {
  '480p': { width: 854, height: 480, label: '480p (854×480)' },
  '720p': { width: 1280, height: 720, label: '720p (1280×720)' },
  '1080p': { width: 1920, height: 1080, label: '1080p (1920×1080)' },
} as const;

type ResolutionKey = keyof typeof RESOLUTIONS;

const RESOLUTION_STORAGE_KEY = 'camlooper.outputResolution';
const DEFAULT_RESOLUTION: ResolutionKey = '720p';

function loadStoredResolution(): ResolutionKey {
  const stored = localStorage.getItem(RESOLUTION_STORAGE_KEY);
  return stored && stored in RESOLUTIONS ? (stored as ResolutionKey) : DEFAULT_RESOLUTION;
}

/// Turn a frame's JPEG bytes into a data URL.
///
/// Chunked on purpose. The obvious `String.fromCharCode(...bytes)` spreads every byte as a
/// separate function argument, which is slow and throws `RangeError: Maximum call stack
/// size exceeded` once a frame exceeds the engine's argument limit — reachable as soon as
/// the output resolution goes above the old hardcoded 640px.
function frameToDataUrl(frame: VideoFrame): string {
  const bytes = frame.data;
  const CHUNK = 0x8000;
  let binary = '';
  for (let i = 0; i < bytes.length; i += CHUNK) {
    binary += String.fromCharCode.apply(null, bytes.slice(i, i + CHUNK) as unknown as number[]);
  }
  return `data:image/jpeg;base64,${btoa(binary)}`;
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
  /** Natural motion buffers the clip before it can start. True for that gap. */
  is_preparing: boolean;
  /** The clip outran the natural-motion window, so the walk covers only its opening. */
  walk_truncated: boolean;
  /** Seconds of footage the walk is actually covering. 0 when not in natural motion. */
  walk_seconds: number;
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
  const { t, i18n } = useTranslation();

  // False while started hidden by "start at login" (tray only); gates the popup and the banner.
  const windowShown = useWindowShown();

  // Keep the document title in sync with the active language.
  useEffect(() => {
    document.title = t("app.title");
  }, [t, i18n.language]);
  const [isPlaying, setIsPlaying] = useState(false);
  const [isVirtualCamActive, setIsVirtualCamActive] = useState(false);
  const [outputResolution, setOutputResolution] = useState<ResolutionKey>(loadStoredResolution);
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
    average_processing_time: 0.0,
    is_preparing: false,
    walk_truncated: false,
    walk_seconds: 0
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
  const [naturalMotion, setNaturalMotion] = useState(loadNaturalMotion);
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

  // Guided flow: which step the wizard is on.
  //  'live'   – set up and start the physical camera
  //  'record' – capture a short clip (optionally an in-progress recording)
  //  'review' – preview the just-recorded clip before committing
  //  'loop'   – the clip is looping to the virtual camera
  type FlowStep = 'live' | 'record' | 'review' | 'loop';
  const [step, setStep] = useState<FlowStep>('live');
  const [showAdvanced, setShowAdvanced] = useState(false);

  // Phone remote (Advanced → Phone remote). Networking lives in Rust: src-tauri/src/remote.
  const remote = useRemote();
  const phoneStream = usePhoneStream({
    // The phone's own Stop: back to idle with a neutral note, never an error.
    onStoppedByPhone: (mode) =>
      toast(
        t(
          mode === 'file'
            ? 'phoneStream.file.cancelledByPhone'
            : mode === 'record'
              ? 'phoneStream.record.cancelledByPhone'
              : mode === 'meeting'
                ? 'phoneStream.meeting.stoppedByPhone'
                : 'phoneStream.stoppedByPhone',
        ),
      ),
    onStopFailed: () => toast.error(t('phoneStream.stopFailed')),
    onPhoneVideoLoaded: (video, playing) => void showPhoneVideo(video, playing),
    onLoopChanged: (playing) => void followPhoneLoop(playing),
    onFileLooping: (_file, deviceName) =>
      toast.success(t('phoneStream.file.looping', { phone: isolate(deviceName || t('phoneStream.yourPhone')) })),
    onFileFailed: (file) =>
      toast.error(t('phoneStream.file.failed'), { description: file.error || undefined }),
    onRecordLooping: (deviceName) =>
      toast.success(t('phoneStream.record.looping', { phone: isolate(deviceName || t('phoneStream.yourPhone')) })),
    onRecordFailed: (error) => toast.error(t('phoneStream.record.failed'), { description: error || undefined }),
  });
  // A phone asking to stream raises the window; that must not also open the welcome page in
  // a browser (which would take the focus away from the question) or count as an app open
  // for the ad. Both wait until the request is answered.
  const phoneRequestPending = phoneStream.request !== null;
  useWelcomePopup(windowShown && !phoneRequestPending);
  const [adAllowed, setAdAllowed] = useState(false);
  useEffect(() => {
    if (windowShown && !phoneRequestPending) setAdAllowed(true);
  }, [windowShown, phoneRequestPending]);
  // While the phone streams, the backend refuses to start playback or the camera; grey the
  // controls and say why instead of letting the click fail.
  const phoneBusy = cameraBusy(phoneStream.status);
  const handlePhoneAccept = async (sessionId: string) => {
    const mode = phoneStream.request?.mode;
    const result = await phoneStream.accept(sessionId);
    if (result === 'expired') {
      toast.error(
        t(
          mode === 'file'
            ? 'phoneStream.file.expired'
            : mode === 'record'
              ? 'phoneStream.record.expired'
              : mode === 'meeting'
                ? 'phoneStream.meeting.expired'
                : 'phoneStream.expired',
        ),
      );
    } else if (result === 'offline') toast.warning(t('phoneStream.offline'));
    else if (result === 'failed') toast.error(t('phoneStream.errors.generic'));
    // 'too_soon': the dialog is still there; nothing to say.
  };
  const meeting = useMeeting();
  const remoteCardRef = React.useRef<RemoteCardHandle>(null);
  const [linkingRequest, setLinkingRequest] = useState(0);
  const [recordSeconds, setRecordSeconds] = useState(0);
  const [recordingPreviewUrl, setRecordingPreviewUrl] = useState<string | null>(null);

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

  const processVideoFile = async (file: File): Promise<boolean> => {
    let success = false;
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
        
        // Auto start if enabled. Use startLoopPlayback (not handlePlayPause)
        // because videoInfo state hasn't updated yet in this render tick.
        if (autoStart) {
          console.log('Auto-starting video playback...');
          await startLoopPlayback();
        }

        success = true;
      } catch (error) {
        console.error('Error uploading video:', error);
        // Map known backend (Rust) error messages to translated copy; fall back
        // to the raw message so nothing is ever swallowed.
        const raw = error instanceof Error ? error.message : String(error);
        const friendly = /unsupported.*format/i.test(raw)
          ? t('errors.unsupportedFormat')
          : t('errors.uploadFailed');
        setUploadError(friendly);
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
    return success;
  };

  // Secondary path: loop a video the student already has on disk. On success we
  // jump straight to the Loop step (the file is already auto-playing).
  const handleFileSelect = async (event: React.ChangeEvent<HTMLInputElement>) => {
    const file = event.target.files?.[0];
    if (file) {
      const ok = await processVideoFile(file);
      if (ok) setStep('loop');
    }
    // Allow re-selecting the same file later.
    event.target.value = '';
  };

  const triggerFileInput = () => {
    const fileInput = document.getElementById('video-file-input') as HTMLInputElement;
    fileInput?.click();
  };

  // Start looping the currently loaded clip to the virtual camera. This does
  // NOT read `videoInfo` state (which lags one render behind processVideoFile's
  // setVideoInfo), so it can be called immediately after a clip is loaded —
  // the clip is already loaded in the backend by then.
  const startLoopPlayback = async () => {
    try {
      // Mutual exclusion: playback and the live camera share the virtual camera
      // sink, so stop the live camera first.
      if (isLiveCamera) {
        await stopLiveCamera();
      }
      // Ensure the virtual camera is running so the loop reaches Zoom even when
      // the clip came from the "loop an existing video" path.
      if (!isVirtualCamActive) {
        await startVirtualCamera();
      }
      await invoke('start_video_stream');
      setIsPlaying(true);
      await startFrameReceiving();
    } catch (error) {
      console.error('Error starting loop playback:', error);
      setIsPlaying(false);
    }
  };

  // The phone sent a video and the backend has already loaded and started it (the same
  // engine as a file picked here) and says so with `remote://phone-video-loaded`, carrying
  // the same VideoInfo a desktop-side load returns. Show it the way a desktop-picked file is
  // shown: loaded, on the Loop step, playing.
  const showPhoneVideo = async (video: LoadedVideo, playing: boolean) => {
    setSelectedVideo(null);
    setVideoInfo(video);
    setVideoMetadata({
      name: video.filename,
      duration: `${Math.floor(video.duration / 60)}:${Math.floor(video.duration % 60).toString().padStart(2, '0')}`,
      dimensions: video.width > 0 && video.height > 0 ? `${video.width}x${video.height}` : '',
      fps: video.fps > 0 ? `${Math.round(video.fps)}fps` : '',
    });
    setUploadError(null);
    setIsLoopingComplete(false);
    // Not out from under a recording that is running.
    if (!isLiveRecording) setStep('loop');
    setIsPlaying(playing);
    if (playing) await startFrameReceiving();
  };

  // "Stop loop" / "Play loop" on the phone (`remote://loop-changed`): the backend has
  // already done it; the controls here follow at once instead of at the next status poll.
  const followPhoneLoop = async (playing: boolean) => {
    if (!videoInfo) return;
    setIsPlaying(playing);
    if (playing) {
      setIsLoopingComplete(false);
      await startFrameReceiving();
    } else {
      frameHolder.current.stop();
      setCurrentFrame(null);
    }
  };

  const handlePlayPause = async () => {
    if (!videoInfo) {
      return;
    }

    if (isPlaying) {
      try {
        await invoke('pause_video_stream');
        setIsPlaying(false);
        frameHolder.current.stop();
      } catch (error) {
        console.error('Error pausing video:', error);
      }
    } else {
      await startLoopPlayback();
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
              // Deliberately not logging per batch. console.log is monkey-patched into
              // React state further up (addLog does a setLogs spread plus a setTimeout),
              // so a log here fired on every single frame and cost more than the frame
              // handling itself.

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
                // Build the data URL once, here, rather than in the render body: the
                // conversion is O(frame size) and rendering re-runs for unrelated state
                // changes too. String.fromCharCode(...arr) also spreads the whole frame as
                // function arguments, which throws RangeError once frames get bigger.
                setCurrentFrame({ ...latestFrame, url: frameToDataUrl(latestFrame) });
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
        
        if (status.is_playing && !isPlaying) {
          // Playback started without a click here: the linked phone sent a video or pressed
          // "Play loop", or a phone camera stream ended and handed the loop back.
          setIsPlaying(true);
          void startFrameReceiving();
        } else if (!status.is_playing && isPlaying) {
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
        autoStart,
        naturalMotion
      }).catch(console.error);
    }
  }, [loopCount, autoStart, naturalMotion, videoInfo]);

  // Persist the output resolution and tell the backend about it. This runs on mount too,
  // so the video processor renders at the right size even when playback starts before the
  // virtual camera is switched on.
  useEffect(() => {
    localStorage.setItem(RESOLUTION_STORAGE_KEY, outputResolution);
    const { width, height } = RESOLUTIONS[outputResolution];
    invoke('set_output_resolution', { width, height }).catch(console.error);
  }, [outputResolution]);

  // Natural motion is the only playback setting with a default that fights the user, so it
  // is the only one that persists.
  useEffect(() => {
    saveNaturalMotion(naturalMotion);
  }, [naturalMotion]);

  // Count up the elapsed recording time while a live recording is in progress.
  useEffect(() => {
    if (!isLiveRecording) return;
    setRecordSeconds(0);
    const id = setInterval(() => setRecordSeconds(s => s + 1), 1000);
    return () => clearInterval(id);
  }, [isLiveRecording]);

  // Release the recorded-clip preview object URL when it changes / on unmount.
  useEffect(() => {
    return () => {
      if (recordingPreviewUrl) URL.revokeObjectURL(recordingPreviewUrl);
    };
  }, [recordingPreviewUrl]);

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
      // On Windows, ensure the softcam DirectShow driver is registered before starting.
      // This is a no-op (no prompt) once registered; on the per-user Microsoft Store build
      // it triggers a single UAC prompt on first use, while the perMachine (direct-download)
      // build already registered it at install time.
      if (navigator.userAgent.includes('Windows')) {
        const outcome = await invoke<string>('ensure_softcam_registered');
        if (outcome === 'declined') {
          toast.warning(t('vcam.driver.declinedTitle', 'Virtual camera driver not enabled'), {
            description: t('vcam.driver.declinedDesc', 'Windows needs administrator approval once to install the CamLooper virtual camera. It won’t appear in Zoom, Meet or OBS until you allow it.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'failed') {
          toast.error(t('vcam.driver.failedTitle', 'Could not install the virtual camera driver'), {
            description: t('vcam.driver.failedDesc', 'Registration failed. Try reinstalling CamLooper, or run it once as administrator.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'justRegistered') {
          toast.success(t('vcam.driver.registeredTitle', 'CamLooper virtual camera installed'), {
            description: t('vcam.driver.registeredDesc', 'You may need to restart Zoom, Meet or OBS for “DirectShow Softcam” to appear in the camera list.'),
          });
        }
      }

      // On Linux, make sure the v4l2loopback kernel module is loaded before starting. This
      // self-heals every install method (apt, dpkg, AppImage): if the module isn't loaded,
      // the app loads it on demand via a one-time polkit prompt. A no-op (no prompt) once a
      // device already exists.
      if (navigator.userAgent.includes('Linux')) {
        const outcome = await invoke<string>('ensure_v4l2loopback');
        if (outcome === 'declined') {
          toast.warning(t('vcam.linux.declinedTitle', 'Virtual camera driver not enabled'), {
            description: t('vcam.linux.declinedDesc', 'CamLooper needs permission once to load its virtual camera driver. It won’t appear in Zoom, Meet or OBS until you allow it.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'notInstalled') {
          toast.error(t('vcam.linux.notInstalledTitle', 'Virtual camera driver missing'), {
            description: t('vcam.linux.notInstalledDesc', 'The v4l2loopback driver isn’t built for your kernel. In a terminal run: sudo apt install linux-headers-$(uname -r) v4l2loopback-dkms && sudo dkms autoinstall — then restart CamLooper.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'secureBootBlocked') {
          toast.error(t('vcam.linux.secureBootTitle', 'Secure Boot is blocking the driver'), {
            description: t('vcam.linux.secureBootDesc', 'Secure Boot won’t let the unsigned v4l2loopback module load. Enroll the DKMS key (sudo mokutil --import …) or disable Secure Boot in your BIOS, then restart CamLooper.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'noPkexec') {
          toast.error(t('vcam.linux.noPkexecTitle', 'Couldn’t load the virtual camera driver'), {
            description: t('vcam.linux.noPkexecDesc', 'CamLooper couldn’t ask for permission to load the driver. Run this once in a terminal: sudo modprobe v4l2loopback — then start the camera again.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'permissionDenied') {
          toast.error(t('vcam.linux.permissionTitle', 'No access to the camera device'), {
            description: t('vcam.linux.permissionDesc', 'Add yourself to the “video” group: sudo usermod -aG video $USER — then log out and back in.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'failed') {
          toast.error(t('vcam.linux.failedTitle', 'Could not enable the virtual camera'), {
            description: t('vcam.linux.failedDesc', 'Loading the v4l2loopback driver failed. Try running sudo modprobe v4l2loopback in a terminal, or reinstall CamLooper.'),
          });
          setIsVirtualCamActive(false);
          return;
        }
        if (outcome === 'justLoaded') {
          toast.success(t('vcam.linux.loadedTitle', 'CamLooper virtual camera ready'), {
            description: t('vcam.linux.loadedDesc', 'You may need to restart Zoom, Meet or OBS for “CamLooper Virtual Camera” to appear in the camera list.'),
          });
        }
        // 'ready' → fall through and start the camera.
      }

      const { width, height } = RESOLUTIONS[outputResolution];
      const config: VirtualCameraConfig = {
        width,
        height,
        fps: 30,
        camera_name: "CamLooper Virtual Camera"
      };

      const status = await invoke<VirtualCameraStatus>('start_virtual_camera', { config });
      setVirtualCameraStatus(status);
      setIsVirtualCamActive(status.is_active);
      console.log('Virtual camera started successfully');
    } catch (error) {
      console.error('Failed to start virtual camera:', error);
      toast.error(t('vcam.startFailedTitle', 'Could not start the virtual camera'), {
        description: String(error),
      });
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

  // A meeting link arrived from the linked phone. Rust already validated the URL against the
  // allow-list and brought the window to the front. Here: get the loop onto the virtual camera
  // (same path as the buttons, prompts and all), open the link, then ack so the server stops
  // re-sending it.
  const handleOpenMeeting = async (cmd: OpenMeetingCommand) => {
    const phone = cmd.mobileDeviceName ?? remote.state.mobileDeviceName ?? t('remote.yourPhone');
    const app = t(`remote.provider.${cmd.provider}`);
    try {
      if (videoInfo) {
        if (!isPlaying) await startLoopPlayback();
        else if (!isVirtualCamActive) await startVirtualCamera();
      } else if (isLiveCamera && !isVirtualCamActive) {
        await startVirtualCamera();
      }
    } catch (error) {
      console.error('remote: could not start the camera before opening the meeting:', error);
    }
    try {
      if (!cmd.url.startsWith('https://')) throw new Error('not an https link');
      const opened = await meeting.open(cmd.url);
      if (opened.mode === 'auto') {
        toast.success(t('remote.autoJoin.joining', { app, phone, browser: opened.browser ?? '' }));
      } else {
        toast.success(t('remote.openingMeeting', { app, phone }));
        if (opened.mode === 'unavailable') {
          toast.info(t('remote.autoJoin.noBrowserTitle'), { description: t('remote.autoJoin.noBrowser') });
        }
      }
      if (!videoInfo && !isLiveCamera) {
        toast.info(t('remote.noLoopHintTitle'), { description: t('remote.noLoopHintDesc') });
      }
    } catch (error) {
      console.error('remote: could not open the meeting link:', error);
      toast.error(t('remote.openFailedTitle'), { description: String(error) });
    } finally {
      try {
        await invoke('remote_ack_command', { commandId: cmd.commandId });
      } catch (error) {
        console.error('remote: ack failed:', error);
      }
    }
  };

  useRemoteCommands({
    onOpenMeeting: handleOpenMeeting,
    onShowLinking: () => {
      setShowAdvanced(true);
      setLinkingRequest((n) => n + 1);
    },
    onLinked: ({ mobileDeviceName, autostartEnabled }) => {
      toast.success(t('remote.toastLinkedTitle', { phone: mobileDeviceName ?? t('remote.yourPhone') }), {
        description: t(autostartEnabled ? 'remote.toastLinkedDesc' : 'remote.toastAutostartFailed'),
      });
    },
    onUnlinkedByPhone: () => {
      toast.info(t('remote.unlinkedByPhoneTitle'), { description: t('remote.unlinkedByPhoneDesc') });
    },
    onRejected: () => {
      toast.warning(t('remote.rejectedTitle'), { description: t('remote.rejectedDesc') });
    },
  });

  // Progress of a driven join (Rust clicks through the pre-join screen in the CamLooper browser).
  useMeetingStatus((status) => {
    const app = t(`remote.provider.${status.provider}`);
    switch (status.phase) {
      case 'joined':
        toast.success(t('remote.autoJoin.joinedTitle', { app }));
        break;
      case 'waiting_admission':
        toast.info(t('remote.autoJoin.admissionTitle'), { description: t('remote.autoJoin.admissionDesc') });
        break;
      case 'blocked':
        toast.warning(t('remote.autoJoin.blockedTitle', { app }), { description: status.detail ?? undefined });
        break;
      case 'timeout':
        toast.warning(t('remote.autoJoin.timeoutTitle'), { description: t('remote.autoJoin.timeoutDesc') });
        break;
      case 'failed':
        toast.error(t('remote.autoJoin.failedTitle'), { description: status.detail ?? undefined });
        break;
      default:
        break;
    }
  });

  const handleOpenMeetingBrowser = async () => {
    try {
      await meeting.openProfile();
    } catch (error) {
      console.error('meeting: could not open the meeting browser:', error);
      toast.error(t('remote.autoJoin.failedTitle'), { description: String(error) });
    }
  };

  // Tray "Link phone…": once Advanced is open, scroll the card into view and focus the code field.
  useEffect(() => {
    if (!linkingRequest) return;
    const id = window.setTimeout(() => {
      remoteCardRef.current?.scrollIntoView();
      remoteCardRef.current?.focusCode();
    }, 50);
    return () => window.clearTimeout(id);
  }, [linkingRequest, showAdvanced]);

  // Keep the tray menu in the UI language.
  useEffect(() => {
    const push = () => {
      void invoke('set_tray_labels', {
        labels: {
          show: t('tray.show'),
          link: t('tray.link'),
          autostart: t('tray.autostart'),
          quit: t('tray.quit'),
          stopPhone: t('tray.stopPhone'),
          phoneLive: t('tray.phoneLive'),
          phoneConnecting: t('tray.phoneConnecting'),
        },
      }).catch(() => {
        /* no tray (e.g. GNOME without the AppIndicator extension) — nothing to label */
      });
    };
    push();
    i18n.on('languageChanged', push);
    return () => {
      i18n.off('languageChanged', push);
    };
  }, [t, i18n]);

  // Initialize virtual camera status
  React.useEffect(() => {
    updateVirtualCameraStatus();
  }, []);

  // --- macOS: Camera Extension approval gate ---
  //
  // macOS will not load the virtual camera until the user switches it on in System
  // Settings. On launch we ask macOS to install it (which opens that Settings pane) and
  // hold the whole app behind an overlay until the approval lands, because nothing the app
  // does is useful to another application before then.
  const [macExtensionPending, setMacExtensionPending] = useState(false);

  React.useEffect(() => {
    if (!navigator.userAgent.includes('Mac')) return;
    // A hidden start at login must not pop System Settings with no window on screen;
    // run the check once the window is actually shown (`windowShown` latches true).
    if (!windowShown) return;
    let cancelled = false;
    invoke<string>('ensure_macos_camera_extension')
      .then(outcome => {
        if (cancelled) return;
        if (outcome === 'needsApproval') {
          setMacExtensionPending(true);
        } else if (outcome === 'failed') {
          toast.error(t('vcam.mac.failedTitle', 'Could not install the CamLooper camera'), {
            description: t('vcam.mac.failedDesc', 'macOS refused the camera extension. Make sure CamLooper is in the Applications folder, then reopen it.'),
          });
        }
        // 'ready' and 'unavailable' (a dev build with no extension) need no gate.
      })
      .catch(error => console.error('Camera extension check failed:', error));
    return () => { cancelled = true; };
  }, [windowShown]);

  React.useEffect(() => {
    if (!macExtensionPending) return;
    const id = setInterval(async () => {
      try {
        if (await invoke<boolean>('macos_camera_extension_approved')) {
          setMacExtensionPending(false);
          toast.success(t('vcam.mac.approvedTitle', 'CamLooper camera approved'), {
            description: t('vcam.mac.approvedDesc', 'Restart Zoom, Meet or OBS if they were open, so “CamLooper Virtual Camera” appears in their camera list.'),
          });
        }
      } catch (error) {
        console.error('Camera extension poll failed:', error);
      }
    }, 1500);
    return () => clearInterval(id);
  }, [macExtensionPending]);

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

  const stopLiveRecording = async (): Promise<string | null> => {
    try {
      const path = await invoke<string>('stop_camera_recording');
      setLastRecordingPath(path);
      setIsLiveRecording(false);
      // Resume the stream-only feed so Zoom isn't interrupted beyond the stop.
      await invoke('start_camera_preview', { deviceId: selectedLiveDevice || null });
      console.log('Live recording saved:', path);
      return path;
    } catch (error) {
      console.error('Failed to stop live recording:', error);
      setIsLiveRecording(false);
      return null;
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
  const applyRecordedClipAsLoop = async (): Promise<boolean> => {
    if (!lastRecordingPath) return false;
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
      return await processVideoFile(file);
    } catch (error) {
      console.error('Failed to load recording as source:', error);
      return false;
    }
  };

  // --- Guided flow orchestration (Live → Record → Loop) ---

  const formatClock = (totalSeconds: number) => {
    const m = Math.floor(totalSeconds / 60);
    const s = totalSeconds % 60;
    return `${m}:${s.toString().padStart(2, '0')}`;
  };

  // Build a playable preview of the just-recorded clip for the review step.
  const loadRecordingPreview = async (path: string) => {
    try {
      const base64 = await invoke<string>('get_recorded_video_base64', { path });
      const binaryString = window.atob(base64);
      const bytes = new Uint8Array(binaryString.length);
      for (let i = 0; i < binaryString.length; i++) {
        bytes[i] = binaryString.charCodeAt(i);
      }
      const url = URL.createObjectURL(new Blob([bytes], { type: 'video/webm' }));
      setRecordingPreviewUrl(url); // previous URL is revoked by the cleanup effect
    } catch (error) {
      console.error('Failed to build recording preview:', error);
    }
  };

  // Step 2: start / stop capturing the clip (camera keeps streaming throughout).
  const handleStartRecording = async () => {
    await startLiveRecording();
  };

  const handleStopRecording = async () => {
    const path = await stopLiveRecording();
    if (path) {
      await loadRecordingPreview(path);
      setStep('review');
    }
  };

  // Review: discard and record again (camera is still live) …
  const handleRetake = () => {
    setRecordingPreviewUrl(null);
    setStep('record');
  };

  // … or commit the clip: swap the live feed for the looping clip.
  const handleUseClip = async () => {
    const ok = await applyRecordedClipAsLoop();
    if (ok) {
      setRecordingPreviewUrl(null);
      setStep('loop');
    }
  };

  // Step 3: go back and capture a fresh loop, restarting the live camera.
  const handleRecordNewLoop = async () => {
    await handleStop();
    setRecordingPreviewUrl(null);
    await startLiveCamera();
    setStep('record');
  };

  // Step 3: fully stop — loop, camera, and virtual camera — back to the start.
  const handleStopEverything = async () => {
    await handleStop();
    if (isLiveCamera) {
      await stopLiveCamera();
    }
    await stopVirtualCamera();
    setRecordingPreviewUrl(null);
    setStep('live');
  };

  // Step 3: momentarily jump between the loop and the real camera without
  // leaving the final screen (e.g. the teacher calls on you), then jump back.
  const handleGoLiveToggle = async (checked: boolean) => {
    if (checked) {
      // Pause the loop and stream the real camera to the virtual camera.
      await startLiveCamera();
    } else {
      // Switch back to the looping clip.
      await stopLiveCamera();
      await startLoopPlayback();
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

  const stepIndex = step === 'live' ? 1 : step === 'loop' ? 3 : 2;

  return (
    <SelectSuppressedContext.Provider value={phoneRequestPending}>
    <div className="min-h-screen bg-background text-foreground">
      <PhoneStreamBanner
        status={phoneStream.status}
        error={phoneStream.error}
        onStop={() => void phoneStream.stop()}
        onDismissError={phoneStream.dismissError}
      />
      {phoneStream.request && (
        <PhoneStreamDialog
          key={requestKey(phoneStream.request)}
          request={phoneStream.request}
          busy={phoneStream.accepting}
          onAccept={(id) => void handlePhoneAccept(id)}
          onDecline={(id) => void phoneStream.reject(id)}
          onExpire={phoneStream.expire}
        />
      )}
      {macExtensionPending && (
        <div
          className="fixed inset-0 z-50 flex items-center justify-center bg-background/95 backdrop-blur-sm p-6"
          role="dialog"
          aria-modal="true"
          aria-labelledby="mac-extension-gate-title"
        >
          <Card className="max-w-md w-full p-6 space-y-4">
            <div className="flex items-center gap-3">
              <ShieldCheck className="h-6 w-6 text-primary shrink-0" />
              <h2 id="mac-extension-gate-title" className="text-lg font-semibold">
                {t('vcam.mac.gateTitle', 'Allow the CamLooper camera')}
              </h2>
            </div>
            <p className="text-sm text-muted-foreground">
              {t('vcam.mac.gateDesc', 'macOS needs your approval before CamLooper can appear as a camera in Zoom, Meet and other apps. System Settings has been opened for you.')}
            </p>
            <ol className="text-sm space-y-2 list-decimal list-inside">
              <li>{t('vcam.mac.gateStep1', 'In System Settings, go to General → Login Items & Extensions.')}</li>
              <li>{t('vcam.mac.gateStep2', 'Under Extensions, click the ⓘ next to Camera Extensions.')}</li>
              <li>{t('vcam.mac.gateStep3', 'Switch on CamLooper and confirm with your password or Touch ID.')}</li>
            </ol>
            <Button
              variant="hero"
              className="w-full"
              onClick={() => invoke('open_macos_extension_settings').catch(console.error)}
            >
              {t('vcam.mac.gateOpenSettings', 'Open System Settings')}
            </Button>
            <p className="flex items-center justify-center gap-2 text-xs text-muted-foreground">
              <Loader2 className="h-3 w-3 animate-spin" />
              {t('vcam.mac.gateWaiting', 'Waiting for your approval — this screen closes by itself.')}
            </p>
          </Card>
        </div>
      )}
      {/* Not mounted while hidden in the tray: an autostart launch is not an app open. */}
      {adAllowed && <AdBanner />}
      <div className="max-w-6xl mx-auto px-4 py-6 space-y-5">
        {/* Top bar: brand + tagline on the left, step indicator on the right */}
        <div className="flex flex-col md:flex-row md:items-center md:justify-between gap-3">
          <div className="space-y-0.5">
            <h1 className="text-xl font-bold inline-flex items-center gap-2">
              <ShieldCheck className="h-5 w-5 text-primary" />
              <span className="bg-gradient-primary bg-clip-text text-transparent">CamLooper</span>
            </h1>
            <p className="text-xs text-muted-foreground max-w-md">
              {t('topbar.tagline')}
            </p>
          </div>
          <div className="flex items-center gap-3">
            <Stepper current={stepIndex} steps={[t('stepper.golive'), t('stepper.record'), t('stepper.loop')]} />
            <Select value={i18n.resolvedLanguage} onValueChange={(lng) => void loadLanguage(lng)}>
              <SelectTrigger className="h-9 w-auto gap-1" aria-label={t('language.label')}>
                <Globe className="h-4 w-4" />
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SUPPORTED_LANGUAGES.map((l) => (
                  <SelectItem key={l.code} value={l.code}>{l.label}</SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        {/* Body: video on the left, the active step's controls on the right */}
        <div className="grid md:grid-cols-[minmax(0,1fr)_22rem] gap-5 items-start">
          {/* Left column: video preview */}
          <div className="space-y-4">
            {/* Shared video stage */}
            <Card className="aspect-video max-h-[52vh] bg-black/50 border-border relative overflow-hidden">
          {step === 'review' && recordingPreviewUrl ? (
            <video
              src={recordingPreviewUrl}
              className="w-full h-full object-cover"
              autoPlay
              loop
              muted
              playsInline
              controls
            />
          ) : isLiveCamera && liveFrame ? (
            <img src={liveFrame} alt={t('stage.altLive')} className="w-full h-full object-cover" />
          ) : isLiveCamera ? (
            <div className="absolute inset-0 flex items-center justify-center bg-black">
              <p className="text-sm text-white/90 font-medium animate-pulse">{t('stage.startingCamera')}</p>
            </div>
          ) : currentFrame ? (
            <img
              src={currentFrame.url ?? frameToDataUrl(currentFrame)}
              alt={t('stage.altLoop')}
              className="w-full h-full object-cover"
            />
          ) : (
            <div className="absolute inset-0 flex items-center justify-center">
              <img src="/splash-screen.png" alt="CamLooper" className="absolute inset-0 w-full h-full object-cover opacity-20" />
              <div className="text-center space-y-3 relative z-10">
                <div className="w-20 h-20 bg-primary/20 backdrop-blur-sm rounded-full flex items-center justify-center mx-auto">
                  <Camera className="h-10 w-10 text-primary" />
                </div>
                <p className="text-sm text-muted-foreground drop-shadow-md">{t('stage.previewPlaceholder')}</p>
              </div>
            </div>
          )}

          {/* LIVE / REC overlay */}
          {isLiveCamera && (
            <div className="absolute top-3 left-3 flex items-center gap-2">
              <Badge variant="secondary" className="bg-red-500/20 text-red-400 border-red-500/40">
                <Radio className="h-3 w-3 mr-1" /> {t('stage.badgeLive')}
              </Badge>
              {isLiveRecording && (
                <Badge variant="secondary" className="bg-red-600/30 text-red-300 border-red-500/50">
                  <Circle className="h-2 w-2 mr-1 fill-current animate-pulse" /> {t('stage.badgeRec', { time: formatClock(recordSeconds) })}
                </Badge>
              )}
            </div>
          )}

          {/* Looping overlay */}
          {isPlaying && (
            <div className="absolute top-3 right-3">
              <Badge variant="secondary" className="bg-primary/20 text-primary border-primary/40">
                <RotateCcw className="h-3 w-3 mr-1" />
                {t('stage.badgeLoop', {
                  current: streamStatus.current_loop + 1,
                  total: streamStatus.loop_count === 10 || streamStatus.loop_count === 0 ? '∞' : streamStatus.loop_count,
                })}
              </Badge>
            </div>
          )}

          {/* Natural motion buffers the clip before the first frame goes out, so say so
              rather than leaving the stage looking hung. */}
          {streamStatus.is_preparing && (
            <div className="absolute top-3 left-3">
              <Badge variant="secondary" className="bg-primary/20 text-primary border-primary/40">
                <Loader2 className="h-3 w-3 me-1 animate-spin" />
                {t('advanced.naturalMotionPreparing')}
              </Badge>
            </div>
          )}

          {/* Loading overlay */}
          {isUploading && (
            <div className="absolute top-3 left-3">
              <Badge variant="secondary" className="bg-blue-500/20 text-blue-400 border-blue-500/40">
                {t('stage.badgeLoading', { progress: uploadProgress })}
              </Badge>
            </div>
          )}
        </Card>

            {/* Hidden canvas used by the frame pipeline */}
            <canvas ref={canvasRef} style={{ display: 'none' }} width={1920} height={1080} />

            {/* Hidden file input for the "loop an existing video" secondary path */}
            <input
              id="video-file-input"
              type="file"
              accept="video/*"
              onChange={handleFileSelect}
              className="hidden"
            />
          </div>

          {/* Right column: controls for the active step */}
          <div className="space-y-4">
        {/* Step 1 — Go live */}
        {step === 'live' && (
          <Card className="p-6 space-y-4">
            <div className="space-y-1">
              <h2 className="text-lg font-semibold">{t('step1.title')}</h2>
              <p className="text-sm text-muted-foreground">
                {t('step1.desc')}
              </p>
            </div>

            {liveDevices.length > 0 ? (
              <div className="space-y-1">
                <label className="text-xs text-muted-foreground">{t('step1.cameraLabel')}</label>
                <Select
                  value={selectedLiveDevice}
                  onValueChange={setSelectedLiveDevice}
                  disabled={isLiveCamera || isLiveLoading}
                >
                  <SelectTrigger className="w-full">
                    <SelectValue placeholder={t('step1.cameraPlaceholder')} />
                  </SelectTrigger>
                  <SelectContent>
                    {liveDevices.map(d => {
                      const path = d.split(':')[0];
                      return <SelectItem key={path} value={path}>{d}</SelectItem>;
                    })}
                  </SelectContent>
                </Select>
              </div>
            ) : (
              <p className="text-sm text-muted-foreground">{t('step1.noCamera')}</p>
            )}

            {!isLiveCamera ? (
              <Button
                variant="hero"
                className="w-full"
                onClick={startLiveCamera}
                disabled={isLiveLoading || liveDevices.length === 0 || phoneBusy}
              >
                {isLiveLoading ? <Loader2 className="h-4 w-4 mr-2 animate-spin" /> : <Camera className="h-4 w-4 mr-2" />}
                {t('step1.startCamera')}
              </Button>
            ) : (
              <Button variant="hero" className="w-full" onClick={() => setStep('record')}>
                {t('step1.next')} <ArrowRight className="h-4 w-4 ml-2" />
              </Button>
            )}

            <button
              type="button"
              onClick={triggerFileInput}
              disabled={phoneBusy}
              className="w-full text-xs text-muted-foreground hover:text-primary transition-colors inline-flex items-center justify-center gap-1 disabled:opacity-40 disabled:pointer-events-none"
            >
              <Upload className="h-3 w-3" /> {t('step1.loopExisting')}
            </button>

            {phoneBusy && (
              <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">{t('phoneStream.controlsLocked')}</p>
            )}
            {uploadError && (
              <div className="p-3 bg-red-500/20 text-red-400 border border-red-500/40 rounded text-sm">
                {uploadError}
              </div>
            )}
          </Card>
        )}

        {/* Step 2 — Record */}
        {step === 'record' && (
          <Card className="p-6 space-y-4">
            <div className="space-y-1">
              <h2 className="text-lg font-semibold">{t('step2.title')}</h2>
              <p className="text-sm text-muted-foreground">
                {t('step2.desc')}
              </p>
            </div>

            {!isLiveRecording ? (
              <Button variant="destructive" className="w-full" onClick={handleStartRecording} disabled={phoneBusy}>
                <Circle className="h-4 w-4 mr-2 fill-current" /> {t('step2.start')}
              </Button>
            ) : (
              <Button variant="hero" className="w-full" onClick={handleStopRecording}>
                <Square className="h-4 w-4 mr-2" /> {t('step2.stop', { time: formatClock(recordSeconds) })}
              </Button>
            )}

            <button
              type="button"
              onClick={() => setStep('live')}
              disabled={isLiveRecording}
              className="w-full text-xs text-muted-foreground hover:text-primary transition-colors disabled:opacity-40"
            >
              {t('step2.back')}
            </button>
            {phoneBusy && (
              <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">{t('phoneStream.controlsLocked')}</p>
            )}
          </Card>
        )}

        {/* Review recorded clip */}
        {step === 'review' && (
          <Card className="p-6 space-y-4">
            <div className="space-y-1">
              <h2 className="text-lg font-semibold">{t('review.title')}</h2>
              <p className="text-sm text-muted-foreground">
                {t('review.desc')}
              </p>
            </div>
            <div className="flex flex-col gap-2">
              <Button variant="outline" className="flex-1" onClick={handleRetake}>
                <RotateCcw className="h-4 w-4 mr-2" /> {t('review.retake')}
              </Button>
              <Button variant="hero" className="flex-1" onClick={handleUseClip} disabled={phoneBusy}>
                {t('review.use')} <ArrowRight className="h-4 w-4 ml-2" />
              </Button>
            </div>
            {phoneBusy && (
              <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">{t('phoneStream.controlsLocked')}</p>
            )}
          </Card>
        )}

        {/* Step 3 — Loop */}
        {step === 'loop' && (
          <Card className="p-6 space-y-5">
            <div className="flex items-start gap-3">
              <div className="mt-0.5 h-8 w-8 rounded-full bg-primary/20 flex items-center justify-center shrink-0">
                <ShieldCheck className="h-4 w-4 text-primary" />
              </div>
              <div className="space-y-1">
                <h2 className="text-lg font-semibold">{t('loop.title')}</h2>
                <p className="text-sm text-muted-foreground">
                  <Trans
                    i18nKey="loop.covered"
                    components={{
                      apps: <span className="text-foreground font-medium" />,
                      device: <span className="text-foreground font-medium" />,
                    }}
                  />
                </p>
              </div>
            </div>

            {phoneBusy && (
              <p className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">{t('phoneStream.controlsLocked')}</p>
            )}

            {/* Jump between the loop and the real camera on the fly */}
            <div className="flex items-center justify-between rounded-lg border border-border p-3">
              <div className="pe-3">
                <div className="text-sm font-medium flex items-center gap-2">
                  <Radio className={cn("h-4 w-4", isLiveCamera ? "text-red-400" : "text-muted-foreground")} />
                  {isLiveCamera ? t('loop.liveOn') : t('loop.liveOff')}
                </div>
                <div className="text-xs text-muted-foreground mt-0.5">
                  {isLiveCamera ? t('loop.liveOnDesc') : t('loop.liveOffDesc')}
                </div>
              </div>
              <Switch
                checked={isLiveCamera}
                onCheckedChange={handleGoLiveToggle}
                disabled={isLiveLoading || phoneBusy}
              />
            </div>

            <div className="flex flex-col gap-2">
              <Button variant="outline" className="flex-1" onClick={handleRecordNewLoop} disabled={phoneBusy}>
                <RotateCcw className="h-4 w-4 mr-2" /> {t('loop.recordNew')}
              </Button>
              <Button variant="ghost" className="flex-1" onClick={handleStopEverything}>
                <Square className="h-4 w-4 mr-2" /> {t('loop.stop')}
              </Button>
            </div>
          </Card>
        )}
          </div>
        </div>

        {/* Advanced (collapsed by default) */}
        <div className="pt-2">
          <button
            type="button"
            onClick={() => setShowAdvanced(v => !v)}
            className="w-full flex items-center justify-center gap-1 text-xs text-muted-foreground hover:text-foreground transition-colors"
          >
            <Settings className="h-3 w-3" /> {t('advanced.toggle')}
            <ChevronDown className={cn("h-3 w-3 transition-transform", showAdvanced && "rotate-180")} />
          </button>

          {showAdvanced && (
            <div className="mt-4 space-y-4">
              {/* Virtual Camera */}
              <Card className="p-4">
                <div className="space-y-4">
                  <div className="flex items-center justify-between">
                    <h3 className="font-semibold flex items-center">
                      <Monitor className="h-4 w-4 mr-2" />
                      {t('advanced.virtualCamera')}
                    </h3>
                    <Badge variant={virtualCameraStatus.is_active ? "default" : "secondary"} className={virtualCameraStatus.is_active ? "bg-green-500/20 text-green-400 border-green-500/40" : ""}>
                      {virtualCameraStatus.is_active ? t('advanced.active') : t('advanced.inactive')}
                    </Badge>
                  </div>

                  <div className="space-y-3">
                    <div className="flex items-center justify-between">
                      <span className="text-sm">{t('advanced.enable')}</span>
                      <Switch
                        checked={isVirtualCamActive}
                        onCheckedChange={handleVirtualCameraToggle}
                        disabled={isVirtualCamLoading || phoneBusy}
                      />
                    </div>

                    <div className="space-y-1.5">
                      <div className="flex items-center justify-between">
                        <span className="text-sm">{t('advanced.resolution', 'Resolution')}</span>
                        <Select
                          value={outputResolution}
                          onValueChange={(v) => setOutputResolution(v as ResolutionKey)}
                          disabled={isVirtualCamActive || isVirtualCamLoading}
                        >
                          <SelectTrigger className="w-[170px] h-8 text-sm">
                            <SelectValue />
                          </SelectTrigger>
                          <SelectContent>
                            {(Object.keys(RESOLUTIONS) as ResolutionKey[]).map((key) => (
                              <SelectItem key={key} value={key}>
                                {RESOLUTIONS[key].label}
                              </SelectItem>
                            ))}
                          </SelectContent>
                        </Select>
                      </div>
                      {isVirtualCamActive && (
                        <p className="text-xs text-muted-foreground">
                          {t('advanced.resolutionLocked', 'Turn the camera off to change this.')}
                        </p>
                      )}
                    </div>

                    <div className="text-xs text-muted-foreground bg-muted/50 p-2 rounded">
                      {t('advanced.statusLine', { status: virtualCameraStatus.is_active ? t('advanced.statusReady') : t('advanced.statusDisabled') })}
                      {virtualCameraStatus.is_active && (
                        <div className="mt-1">
                          <div>{t('advanced.resolutionLine', { value: virtualCameraStatus.resolution })}</div>
                          <div>{t('advanced.fpsLine', { value: virtualCameraStatus.fps })}</div>
                          <div>{t('advanced.framesLine', { value: virtualCameraStatus.frame_count })}</div>
                        </div>
                      )}
                    </div>

                  </div>
                </div>
              </Card>

              {/* Phone remote */}
              <RemoteCard
                ref={remoteCardRef}
                state={remote.state}
                busy={remote.busy}
                onLink={remote.link}
                onUnlink={remote.unlink}
                onToggleAutostart={remote.setAutostart}
                meeting={{ browserName: meeting.browser.name, autoJoin: meeting.autoJoin, displayName: meeting.displayName }}
                onToggleAutoJoin={meeting.setAutoJoin}
                onChangeDisplayName={meeting.setDisplayName}
                onOpenMeetingBrowser={handleOpenMeetingBrowser}
              />

              {/* Loop settings */}
              <Card className="p-4">
                <div className="space-y-4">
                  <h3 className="font-semibold flex items-center">
                    <RotateCcw className="h-4 w-4 mr-2" />
                    {t('advanced.loopSettings')}
                  </h3>

                  <div>
                    <div className="flex items-center justify-between mb-2">
                      <span className="text-sm">{t('advanced.numberOfLoops')}</span>
                      <span className="text-sm text-primary">
                        {loopCount[0] === 10 ? t('advanced.forever') : t('advanced.loopTimes', { count: loopCount[0] })}
                      </span>
                    </div>
                    <Slider value={loopCount} onValueChange={setLoopCount} max={10} min={1} step={1} className="w-full" />
                    <div className="flex justify-between text-xs text-muted-foreground mt-1">
                      <span>{t('advanced.once')}</span>
                      <span>{t('advanced.forever')}</span>
                    </div>
                  </div>

                  <div className="flex items-start justify-between gap-3">
                    <div className="min-w-0">
                      <div className="text-sm">{t('advanced.naturalMotion')}</div>
                      <p className="text-xs text-muted-foreground mt-0.5">
                        {t('advanced.naturalMotionDesc')}
                      </p>
                      {isPlaying && (
                        <p className="text-xs text-muted-foreground mt-1">
                          {t('advanced.naturalMotionRestart')}
                        </p>
                      )}
                      {streamStatus.walk_truncated && (
                        <p className="text-xs text-amber-500 mt-1">
                          {t('advanced.naturalMotionTruncated', {
                            seconds: Math.round(streamStatus.walk_seconds)
                          })}
                        </p>
                      )}
                    </div>
                    <Switch checked={naturalMotion} onCheckedChange={setNaturalMotion} />
                  </div>

                  <div className="flex items-center justify-between">
                    <span className="text-sm">{t('advanced.autoplay')}</span>
                    <Switch checked={autoStart} onCheckedChange={setAutoStart} />
                  </div>
                </div>
              </Card>

              {/* Video Information */}
              {videoInfo && (
                <Card className="p-4">
                  <div className="space-y-4">
                    <h3 className="font-semibold">{t('advanced.videoInfo')}</h3>
                    <div className="space-y-2 text-sm">
                      <div className="flex justify-between">
                        <span className="text-muted-foreground">{t('advanced.videoName')}</span>
                        <span>{videoInfo.filename}</span>
                      </div>
                      <div className="flex justify-between">
                        <span className="text-muted-foreground">{t('advanced.videoDuration')}</span>
                        <span>{Math.floor(videoInfo.duration / 60)}:{Math.floor(videoInfo.duration % 60).toString().padStart(2, '0')}</span>
                      </div>
                      {/* Left out when the backend could not tell (a payload without a frame size). */}
                      {videoInfo.width > 0 && videoInfo.height > 0 && (
                        <div className="flex justify-between">
                          <span className="text-muted-foreground">{t('advanced.videoResolution')}</span>
                          <span>{videoInfo.width}x{videoInfo.height}</span>
                        </div>
                      )}
                      {videoInfo.fps > 0 && (
                        <div className="flex justify-between">
                          <span className="text-muted-foreground">{t('advanced.videoFps')}</span>
                          <span>{Math.round(videoInfo.fps)}</span>
                        </div>
                      )}
                      <div className="flex justify-between">
                        <span className="text-muted-foreground">{t('advanced.videoFormat')}</span>
                        <span>{videoInfo.format}</span>
                      </div>
                    </div>
                  </div>
                </Card>
              )}

              {/* Performance Metrics */}
              <Card className="p-4">
                <div className="space-y-4">
                  <div className="flex items-center justify-between">
                    <h3 className="font-semibold text-sm">{t('advanced.perfMetrics')}</h3>
                    <Switch checked={showPerformanceMetrics} onCheckedChange={setShowPerformanceMetrics} />
                  </div>
                  {showPerformanceMetrics && (
                    <div className="grid grid-cols-2 gap-4 text-xs">
                      <div>
                        <span className="text-muted-foreground">{t('advanced.framesProcessed')}</span>
                        <span className="ms-2 font-mono">{performanceMetrics.frames_processed}</span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">{t('advanced.framesDropped')}</span>
                        <span className="ms-2 font-mono">{performanceMetrics.frames_dropped}</span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">{t('advanced.encodeTime')}</span>
                        <span className="ms-2 font-mono">{performanceMetrics.average_encode_time.toFixed(2)}ms</span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">{t('advanced.currentQuality')}</span>
                        <span className="ms-2 font-mono">{performanceMetrics.current_quality}%</span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">{t('advanced.statusLabel')}</span>
                        <span className="ms-2 font-mono">{isPlaying ? t('advanced.playing') : t('advanced.stopped')}</span>
                      </div>
                      <div>
                        <span className="text-muted-foreground">{t('advanced.videoFps')}</span>
                        <span className="ms-2 font-mono">{streamStatus.actual_fps.toFixed(1)}/{streamStatus.target_fps.toFixed(0)}</span>
                      </div>
                    </div>
                  )}
                </div>
              </Card>

              {/* Debug Logs */}
              <Card className="p-4">
                <div className="space-y-4">
                  <div className="flex items-center justify-between">
                    <h3 className="font-semibold flex items-center">
                      <FileText className="h-4 w-4 mr-2" />
                      <span className="text-sm">{t('advanced.debugLogs')}</span>
                      <Badge variant="secondary" className="ms-2">
                        {logs.length}
                      </Badge>
                    </h3>
                    <div className="flex items-center space-x-2">
                      <Button variant="ghost" size="sm" onClick={exportLogs} disabled={logs.length === 0}>
                        <Download className="h-3 w-3 mr-1" />
                        {t('advanced.export')}
                      </Button>
                      <Button variant="ghost" size="sm" onClick={clearLogs} disabled={logs.length === 0}>
                        <Trash2 className="h-3 w-3 mr-1" />
                        {t('advanced.clear')}
                      </Button>
                      <Button variant="ghost" size="sm" onClick={() => setShowLogs(!showLogs)}>
                        {showLogs ? <EyeOff className="h-3 w-3 mr-1" /> : <Eye className="h-3 w-3 mr-1" />}
                        {showLogs ? t('advanced.hide') : t('advanced.show')}
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
                        placeholder={t('advanced.logsPlaceholder')}
                        style={{
                          fontFamily: 'ui-monospace, SFMono-Regular, "SF Mono", Consolas, "Liberation Mono", Menlo, monospace',
                          lineHeight: '1.4'
                        }}
                      />
                      <div className="absolute bottom-2 right-2 text-xs text-gray-500">
                        {t('advanced.logEntries', { count: logs.length })}
                      </div>
                    </div>
                  )}
                </div>
              </Card>
            </div>
          )}
        </div>
      </div>
    </div>
    </SelectSuppressedContext.Provider>
  );
}

const App = () => (
  <TooltipProvider>
    <Toaster />
    <Sonner />
    <CamLooper />
  </TooltipProvider>
);

export default App;
