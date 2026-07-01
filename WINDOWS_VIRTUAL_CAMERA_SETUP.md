# Windows Virtual Camera Setup Guide

## 🚀 Quick Start (5 minutes)

### Step 1: Download Softcam
1. Go to: https://github.com/tshino/softcam/releases
2. Download the latest `softcam-X.X.X-windows-x64.zip`
3. Extract to a folder (e.g., `C:\softcam\`)

### Step 2: Register the DLL
1. **Open Command Prompt as Administrator**
2. **Navigate to softcam folder**: `cd C:\softcam\`
3. **Register the DLL**: `regsvr32 softcam.dll`
4. **You should see**: "DllRegisterServer in softcam.dll succeeded"

### Step 3: Verify Installation
1. **Open Windows Camera app**
2. **Check device list** - you should see "DirectShow Softcam"
3. **If not visible**: Restart Windows Camera app

### Step 4: Test CamLooper
1. **Build your project**: `cargo build`
2. **Run CamLooper**: Your virtual camera should now work!
3. **Check logs**: Look for "Created softcam virtual camera" messages

## ✅ Verification Steps

### Test with Different Apps
- **Windows Camera**: Should list "DirectShow Softcam"
- **Discord**: Video settings → Camera → Select virtual camera
- **Zoom**: Video settings → Camera → Select virtual camera  
- **OBS Studio**: Add Video Capture Device → Select virtual camera

### Expected Log Messages
```
Loaded softcam DLL from: softcam.dll
Loaded all softcam function pointers successfully
Created softcam virtual camera: 1920x1080 @ 30fps
```

## 🚨 Troubleshooting

### "DLL not found"
```bash
# Try different locations
regsvr32 "C:\full\path\to\softcam.dll"
```

### "Access denied"
- **Solution**: Run Command Prompt as Administrator
- **Check**: Right-click → "Run as administrator"

### Camera not visible in apps
```bash
# Re-register the DLL
regsvr32 /u softcam.dll
regsvr32 softcam.dll
```

### Build errors
```bash
# Clean and rebuild
cargo clean
cargo build
```

## 📝 What Happens Next

1. **Your Rust code** automatically detects and uses softcam
2. **Dynamic loading** means it works even if softcam isn't installed
3. **DirectShow integration** makes it visible to all Windows apps
4. **Real-time streaming** sends your video frames to applications

## 🎯 Success Indicators

- ✅ **regsvr32 succeeds** without errors
- ✅ **Windows Camera app** shows virtual camera in device list
- ✅ **CamLooper logs** show "Created softcam virtual camera"
- ✅ **Test apps** can select and use the virtual camera
- ✅ **Video displays** correctly in real-time

## 📞 Need Help?

If you encounter issues:

1. **Check the full documentation**: `docs/WINDOWS_VIRTUAL_CAMERA_IMPLEMENTATION.md`
2. **Verify softcam version**: Use latest release
3. **Check Windows version**: Requires Windows 10+
4. **Try 32-bit version**: If using 32-bit applications

**Once working, your virtual camera will be available system-wide! 🎉** 