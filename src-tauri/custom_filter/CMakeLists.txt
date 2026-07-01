cmake_minimum_required(VERSION 3.16)
project(CustomVideoSource)

# Set C++ standard
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# Set output directories
set(CMAKE_RUNTIME_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/bin)
set(CMAKE_LIBRARY_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)
set(CMAKE_ARCHIVE_OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/lib)

# Find required packages
find_package(PkgConfig REQUIRED)

# DirectShow SDK paths (adjust these for your system)
set(DIRECTSHOW_SDK_PATH "C:/Program Files (x86)/Microsoft DirectX SDK (June 2010)")
set(DIRECTSHOW_INCLUDE_PATH "${DIRECTSHOW_SDK_PATH}/Include")
set(DIRECTSHOW_LIB_PATH "${DIRECTSHOW_SDK_PATH}/Lib/x86")

# Include directories
include_directories(
    ${CMAKE_CURRENT_SOURCE_DIR}
    ${DIRECTSHOW_INCLUDE_PATH}
    ${CMAKE_CURRENT_SOURCE_DIR}/include
)

# Link directories
link_directories(${DIRECTSHOW_LIB_PATH})

# Source files
set(SOURCES
    CustomVideoSource.cpp
)

# Create the DLL
add_library(CustomVideoSource SHARED ${SOURCES})

# Set target properties
set_target_properties(CustomVideoSource PROPERTIES
    OUTPUT_NAME "CustomVideoSource"
    PREFIX ""
    SUFFIX ".ax"  # DirectShow filter extension
)

# Link libraries
target_link_libraries(CustomVideoSource
    strmiids.lib
    quartz.lib
    ole32.lib
    oleaut32.lib
    uuid.lib
)

# Compiler definitions
target_compile_definitions(CustomVideoSource PRIVATE
    UNICODE
    _UNICODE
    WIN32
    _WIN32
    _WIN32_WINNT=0x0601
    WINVER=0x0601
)

# Installation
install(TARGETS CustomVideoSource
    RUNTIME DESTINATION bin
    LIBRARY DESTINATION lib
)

# Registration script
configure_file(
    ${CMAKE_CURRENT_SOURCE_DIR}/register_filter.bat.in
    ${CMAKE_CINARY_DIR}/register_filter.bat
    @ONLY
)

# Build script for registration
add_custom_target(register_filter ALL
    COMMAND ${CMAKE_CURRENT_BINARY_DIR}/register_filter.bat
    DEPENDS CustomVideoSource
    COMMENT "Registering DirectShow filter..."
)

# Alternative: Use regsvr32 directly
add_custom_target(register_direct ALL
    COMMAND regsvr32 /s ${CMAKE_CURRENT_BINARY_DIR}/bin/CustomVideoSource.ax
    DEPENDS CustomVideoSource
    COMMENT "Registering DirectShow filter with regsvr32..."
)

# Unregister target
add_custom_target(unregister_filter
    COMMAND regsvr32 /u /s ${CMAKE_CURRENT_BINARY_DIR}/bin/CustomVideoSource.ax
    COMMENT "Unregistering DirectShow filter..."
) 