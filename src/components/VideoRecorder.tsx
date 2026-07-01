import React, { useState, useEffect } from 'react';
import { Button } from './ui/button';
import { Dialog, DialogContent, DialogHeader, DialogTitle, DialogTrigger } from './ui/dialog';
import { Video, Square, Play, AlertCircle } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { convertFileSrc } from '@tauri-apps/api/core';

interface VideoRecorderProps {
  onRecordingComplete: (file: File) => void;
}

export function VideoRecorder({ onRecordingComplete }: VideoRecorderProps) {
  const [isOpen, setIsOpen] = useState(false);
  const [isRecording, setIsRecording] = useState(false);
  const [error, setError] = useState<string | null>(null);
  
  const [frameData, setFrameData] = useState<string | null>(null);
  const [recordedUrl, setRecordedUrl] = useState<string | null>(null);
  const [recordedFile, setRecordedFile] = useState<File | null>(null);
  const [frameCount, setFrameCount] = useState(0);
  const [debugStatus, setDebugStatus] = useState("Initializing...");
  
  const [devices, setDevices] = useState<string[]>([]);
  const [selectedDevice, setSelectedDevice] = useState<string>('');

  // Fetch devices when dialog opens
  useEffect(() => {
    if (isOpen) {
      invoke<string[]>('list_video_devices')
        .then(devs => {
          setDevices(devs);
          if (devs.length > 0 && !selectedDevice) {
            setSelectedDevice(devs[0].split(':')[0]);
          }
        })
        .catch(console.error);
    } else {
      setSelectedDevice('');
    }
  }, [isOpen]);

  useEffect(() => {
    let isActive = false;

    const pollFrames = async () => {
      while (isActive) {
        try {
          const frame = await invoke<string>('get_camera_preview_frame');
          setFrameCount(c => c + 1);
          setFrameData(`data:image/jpeg;base64,${frame}`);
          setDebugStatus("Receiving frames dynamically!");
        } catch (e) {
          // Ignore errors like "No frame available" if ffmpeg is still starting
        }
        await new Promise(r => setTimeout(r, 1000 / 30));
      }
    };

    const startPreview = async () => {
      try {
        setError(null);
        setFrameCount(0);
        setFrameData(null);
        
        setDebugStatus(`Starting camera ${selectedDevice || 'default'}...`);
        await invoke('start_camera_preview', { deviceId: selectedDevice || null });
        
        setDebugStatus("Backend started. Pulling frames...");
        isActive = true;
        pollFrames();
      } catch (err) {
        setDebugStatus("Failed!");
        setError(typeof err === 'string' ? err : 'Failed to start camera preview');
      }
    };

    if (isOpen && !recordedUrl) {
      startPreview();
    } else if (!isOpen) {
      invoke('stop_camera_preview').catch(console.error);
    }

    return () => {
      isActive = false;
      invoke('stop_camera_preview').catch(console.error);
    };
  }, [isOpen, selectedDevice, recordedUrl]);

  const startRecording = async () => {
    try {
      setError(null);
      await invoke('start_camera_recording', { deviceId: selectedDevice || null });
      setIsRecording(true);
    } catch (err) {
      setError(typeof err === 'string' ? err : 'Error starting recording');
      setIsRecording(false);
    }
  };

  const stopRecording = async () => {
    try {
      const path = await invoke<string>('stop_camera_recording');
      setIsRecording(false);
      
      const base64 = await invoke<string>('get_recorded_video_base64', { path });
      const binaryString = window.atob(base64);
      const len = binaryString.length;
      const bytes = new Uint8Array(len);
      for (let i = 0; i < len; i++) {
        bytes[i] = binaryString.charCodeAt(i);
      }
      
      const blob = new Blob([bytes], { type: 'video/webm' });
      const url = URL.createObjectURL(blob);
      setRecordedUrl(url);
      
      const file = new File([blob], 'recording.webm', { type: 'video/webm' });
      setRecordedFile(file);
      
    } catch (err) {
      setError(typeof err === 'string' ? err : 'Error stopping recording');
      setIsRecording(false);
    }
  };

  const handleUseRecording = () => {
    if (!recordedFile) return;
    onRecordingComplete(recordedFile);
    setIsOpen(false);
  };

  return (
    <Dialog open={isOpen} onOpenChange={setIsOpen}>
      <DialogTrigger asChild>
        <Button variant="outline" size="sm" className="w-full">
          <Video className="h-4 w-4 mr-2" />
          Record New Video
        </Button>
      </DialogTrigger>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Record New Video</DialogTitle>
        </DialogHeader>
        
        <div className="flex flex-col space-y-4">
          {error && (
            <div className="flex items-center space-x-2 text-sm text-red-500 bg-red-500/10 p-2 rounded">
              <AlertCircle className="h-4 w-4" />
              <span>{error}</span>
            </div>
          )}

          {devices.length > 0 && !recordedUrl && (
            <select 
              className="w-full p-2 bg-gray-900 border border-gray-800 rounded-md text-sm text-white focus:outline-none focus:ring-2 focus:ring-purple-500"
              value={selectedDevice}
              onChange={(e) => setSelectedDevice(e.target.value)}
              disabled={isRecording}
            >
              {devices.map(d => {
                const path = d.split(':')[0];
                return <option key={path} value={path}>{d}</option>;
              })}
            </select>
          )}
          
          <div className="relative aspect-video bg-black rounded-lg overflow-hidden">
            {recordedUrl ? (
              <video src={recordedUrl} controls className="w-full h-full object-cover" />
            ) : frameData ? (
              <img src={frameData} alt="Camera Preview" className="w-full h-full object-cover" />
            ) : (
              <div className="w-full h-full flex flex-col items-center justify-center text-gray-500">
                <span>Starting camera...</span>
                <span className="text-xs mt-2">Status: {debugStatus}</span>
                <span className="text-xs mt-1">Frames received: {frameCount}</span>
              </div>
            )}
            
            {isRecording && (
              <div className="absolute top-2 right-2 flex items-center space-x-2 bg-black/50 rounded px-2 py-1">
                <div className="w-2 h-2 rounded-full bg-red-500 animate-pulse" />
                <span className="text-xs text-white">Recording</span>
              </div>
            )}
          </div>

          <div className="flex justify-center space-x-4">
            {!recordedUrl ? (
              isRecording ? (
                <Button variant="destructive" onClick={stopRecording}>
                  <Square className="h-4 w-4 mr-2" />
                  Stop Recording
                </Button>
              ) : (
                <Button onClick={startRecording} disabled={!!error}>
                  <Play className="h-4 w-4 mr-2" />
                  Start Recording
                </Button>
              )
            ) : (
              <div className="flex space-x-2 w-full">
                <Button 
                  variant="outline" 
                  className="flex-1" 
                  onClick={() => { 
                    setRecordedUrl(null); 
                    setRecordedFile(null);
                    // Resume preview
                    invoke('start_camera_preview').catch(console.error);
                  }}
                >
                  Retake
                </Button>
                <Button className="flex-1" onClick={handleUseRecording} disabled={!recordedFile}>
                  Use Recording
                </Button>
              </div>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  );
}
