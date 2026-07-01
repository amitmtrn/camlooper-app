@echo off
echo Building CamLooper Custom Video Source Filter...

REM Check if Visual Studio is available
where cl >nul 2>&1
if %errorLevel% neq 0 (
    echo Visual Studio compiler not found in PATH.
    echo Please run this from a Visual Studio Developer Command Prompt.
    echo Or run: "C:\Program Files (x86)\Microsoft Visual Studio\2019\Community\VC\Auxiliary\Build\vcvars32.bat"
    pause
    exit /b 1
)

REM Create build directory
if not exist build mkdir build
cd build

REM Configure with CMake
cmake .. -G "Visual Studio 16 2019" -A Win32

if %errorLevel% neq 0 (
    echo CMake configuration failed.
    pause
    exit /b 1
)

REM Build the project
cmake --build . --config Release

if %errorLevel% neq 0 (
    echo Build failed.
    pause
    exit /b 1
)

echo.
echo Build completed successfully!
echo The filter DLL is located at: build\bin\Release\CustomVideoSource.ax
echo.
echo To register the filter, run as administrator:
echo   regsvr32 build\bin\Release\CustomVideoSource.ax
echo.

pause 