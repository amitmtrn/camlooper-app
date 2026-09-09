#!/usr/bin/env python3
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
RELEASE_DIR = os.path.join(ROOT, "src-tauri", "target", "x86_64-pc-windows-msvc", "release")
MSI_OUT_DIR = os.path.join(RELEASE_DIR, "bundle", "msi")
os.makedirs(MSI_OUT_DIR, exist_ok=True)

WXS_PATH = os.path.join(MSI_OUT_DIR, "camlooper-store.wxs")
WIXOBJ_PATH = os.path.join(MSI_OUT_DIR, "camlooper-store.wixobj")
MSI_PATH = os.path.join(MSI_OUT_DIR, "camlooper_0.2.4_x64-store.msi")

WXS_CONTENT = """<?xml version="1.0" encoding="UTF-8"?>
<Wix xmlns="http://schemas.microsoft.com/wix/2006/wi">
  <Product Id="*" Name="CamLooper" Language="1033" Version="0.2.4.0" Manufacturer="CamLooper" UpgradeCode="8D9B4A7C-1234-4567-8901-ABCDEF123456">
    <Package InstallerVersion="200" Compressed="yes" InstallScope="perUser" InstallPrivileges="limited" SummaryCodepage="1252" />
    <MediaTemplate EmbedCab="yes" />

    <Directory Id="TARGETDIR" Name="SourceDir">
      <Directory Id="LocalAppDataFolder">
        <Directory Id="INSTALLDIR" Name="CamLooper">
          <Component Id="MainExecutable" Guid="{A1B2C3D4-E5F6-4701-8901-234567890123}">
            <File Id="CamLooperExe" Source="camlooper.exe" KeyPath="yes">
              <Shortcut Id="ApplicationStartMenuShortcut" Directory="ProgramMenuFolder" Name="CamLooper" Description="CamLooper Virtual Camera" WorkingDirectory="INSTALLDIR" Icon="AppIcon.exe" IconIndex="0" Advertise="yes" />
            </File>
            <RemoveFolder Id="CleanUpInstallDir" On="uninstall" />
            <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="installed" Type="integer" Value="1" KeyPath="no" />
          </Component>
          <Component Id="SoftcamDll" Guid="{B2C3D4E5-F6A7-4802-9012-345678901234}">
            <File Id="SoftcamDllFile" Source="softcam.dll" KeyPath="yes" />
            <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="softcam" Type="integer" Value="1" KeyPath="no" />
          </Component>
          <Component Id="SoftcamLicense" Guid="{C3D4E5F6-A7B8-4903-0123-456789012345}">
            <File Id="SoftcamLicenseFile" Source="softcam-LICENSE.txt" KeyPath="yes" />
            <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="softcam_license" Type="integer" Value="1" KeyPath="no" />
          </Component>
          <Directory Id="FFmpegDir" Name="ffmpeg">
            <Component Id="FFmpegExe" Guid="{D4E5F6A7-B8C9-4A04-1234-567890123456}">
              <File Id="FFmpegExeFile" Source="ffmpeg/ffmpeg.exe" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="ffmpeg" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="AvcodecDll" Guid="{E5F6A7B8-C9D0-4B05-2345-678901234567}">
              <File Id="AvcodecDllFile" Source="ffmpeg/avcodec-63.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="avcodec" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="AvdeviceDll" Guid="{F6A7B8C9-D0E1-4C06-3456-789012345678}">
              <File Id="AvdeviceDllFile" Source="ffmpeg/avdevice-63.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="avdevice" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="AvfilterDll" Guid="{A7B8C9D0-E1F2-4D07-4567-890123456789}">
              <File Id="AvfilterDllFile" Source="ffmpeg/avfilter-12.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="avfilter" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="AvformatDll" Guid="{B8C9D0E1-F2A3-4E08-5678-901234567890}">
              <File Id="AvformatDllFile" Source="ffmpeg/avformat-63.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="avformat" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="AvutilDll" Guid="{C9D0E1F2-A3B4-4F09-6789-012345678901}">
              <File Id="AvutilDllFile" Source="ffmpeg/avutil-61.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="avutil" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="SwresampleDll" Guid="{D0E1F2A3-B4C5-4010-7890-123456789012}">
              <File Id="SwresampleDllFile" Source="ffmpeg/swresample-7.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="swresample" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="SwscaleDll" Guid="{E1F2A3B4-C5D6-4111-8901-234567890123}">
              <File Id="SwscaleDllFile" Source="ffmpeg/swscale-10.dll" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="swscale" Type="integer" Value="1" KeyPath="no" />
            </Component>
            <Component Id="FFmpegLicense" Guid="{F2A3B4C5-D6E7-4212-9012-345678901234}">
              <File Id="FFmpegLicenseFile" Source="ffmpeg/LICENSE.txt" KeyPath="yes" />
              <RegistryValue Root="HKCU" Key="Software\\CamLooper" Name="ffmpeg_license" Type="integer" Value="1" KeyPath="no" />
            </Component>
          </Directory>
        </Directory>
      </Directory>
      <Directory Id="ProgramMenuFolder" />
    </Directory>

    <Icon Id="AppIcon.exe" SourceFile="camlooper.exe" />

    <Feature Id="MainApplication" Title="CamLooper" Level="1">
      <ComponentRef Id="MainExecutable" />
      <ComponentRef Id="SoftcamDll" />
      <ComponentRef Id="SoftcamLicense" />
      <ComponentRef Id="FFmpegExe" />
      <ComponentRef Id="AvcodecDll" />
      <ComponentRef Id="AvdeviceDll" />
      <ComponentRef Id="AvfilterDll" />
      <ComponentRef Id="AvformatDll" />
      <ComponentRef Id="AvutilDll" />
      <ComponentRef Id="SwresampleDll" />
      <ComponentRef Id="SwscaleDll" />
      <ComponentRef Id="FFmpegLicense" />
    </Feature>
  </Product>
</Wix>
"""

WXS_REL = os.path.join("bundle", "msi", "camlooper-store.wxs")
WIXOBJ_REL = os.path.join("bundle", "msi", "camlooper-store.wixobj")
MSI_REL = os.path.join("bundle", "msi", "camlooper_0.2.4_x64-store.msi")

with open(WXS_PATH, "w", encoding="utf-8") as f:
    f.write(WXS_CONTENT)

print(f"Wrote WiX schema to {WXS_PATH}")

env = os.environ.copy()
env["WINEDEBUG"] = "-all"

# 1. Run candle.exe
cmd_candle = ["wine", "/tmp/wix311/candle.exe", "-arch", "x64", "-out", WIXOBJ_REL, WXS_REL]
print(f"Compiling WiX XML with candle.exe...")
res1 = subprocess.run(cmd_candle, cwd=RELEASE_DIR, env=env)
if res1.returncode != 0:
    print("candle.exe failed!")
    sys.exit(res1.returncode)

# 2. Run light.exe with -sval to skip Windows-only ICE validation under Wine
cmd_light = ["wine", "/tmp/wix311/light.exe", "-sval", "-out", MSI_REL, WIXOBJ_REL]
print(f"Linking MSI with light.exe...")
res2 = subprocess.run(cmd_light, cwd=RELEASE_DIR, env=env)
if res2.returncode != 0:
    print("light.exe failed!")
    sys.exit(res2.returncode)

print(f"Successfully generated Store MSI package at: {MSI_PATH}")

