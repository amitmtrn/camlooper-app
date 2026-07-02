// Cross-compilation version of CustomVideoSource
// This is a simplified version that can be built with MinGW-w64

#include <windows.h>
#include <iostream>
#include <vector>
#include <string>
// Must come after <windows.h> and before the DEFINE_GUID lines so the custom GUIDs
// below are actually allocated in this translation unit. Without it they are only
// extern declarations and QueryInterface's IID_ICustomVideoSource reference fails
// to link once the archive is properly linked into the final binary.
#include <initguid.h>

// Custom filter GUIDs
DEFINE_GUID(CLSID_CustomVideoSource, 
    0x12345678, 0x1234, 0x1234, 0x12, 0x34, 0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC);

DEFINE_GUID(IID_ICustomVideoSource, 
    0x87654321, 0x4321, 0x4321, 0x43, 0x21, 0xCB, 0xA9, 0x87, 0x65, 0x43, 0x21);

// Custom interface for setting frame data
DECLARE_INTERFACE_(ICustomVideoSource, IUnknown)
{
    STDMETHOD(QueryInterface)(THIS_ REFIID riid, void** ppv) PURE;
    STDMETHOD_(ULONG, AddRef)(THIS) PURE;
    STDMETHOD_(ULONG, Release)(THIS) PURE;
    
    // Custom methods
    STDMETHOD(SetFrameData)(THIS_ const BYTE* data, DWORD size) PURE;
    STDMETHOD(SetResolution)(THIS_ DWORD width, DWORD height) PURE;
    STDMETHOD(SetFramerate)(THIS_ DWORD fps) PURE;
};

// Simplified custom video source filter class
class CCustomVideoSourceFilter : public ICustomVideoSource
{
public:
    CCustomVideoSourceFilter() : m_cRef(1), m_width(1920), m_height(1080), m_fps(30) {
        m_frameData.resize(m_width * m_height * 3, 0);
        InitializeCriticalSection(&m_csFrameData);
    }
    
    virtual ~CCustomVideoSourceFilter() {
        DeleteCriticalSection(&m_csFrameData);
    }

    // IUnknown methods
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv) {
        if (riid == IID_IUnknown || riid == IID_ICustomVideoSource) {
            *ppv = static_cast<ICustomVideoSource*>(this);
            AddRef();
            return S_OK;
        }
        *ppv = NULL;
        return E_NOINTERFACE;
    }

    STDMETHODIMP_(ULONG) AddRef() {
        return InterlockedIncrement(&m_cRef);
    }

    STDMETHODIMP_(ULONG) Release() {
        LONG cRef = InterlockedDecrement(&m_cRef);
        if (cRef == 0) {
            delete this;
        }
        return cRef;
    }

    // ICustomVideoSource methods
    STDMETHODIMP SetFrameData(const BYTE* data, DWORD size) {
        if (!data || size == 0) {
            return E_INVALIDARG;
        }
        
        EnterCriticalSection(&m_csFrameData);
        
        if (size != m_width * m_height * 3) {
            LeaveCriticalSection(&m_csFrameData);
            return E_INVALIDARG;
        }
        
        m_frameData.assign(data, data + size);
        
        LeaveCriticalSection(&m_csFrameData);
        return S_OK;
    }

    STDMETHODIMP SetResolution(DWORD width, DWORD height) {
        if (width == 0 || height == 0) {
            return E_INVALIDARG;
        }
        
        EnterCriticalSection(&m_csFrameData);
        m_width = width;
        m_height = height;
        m_frameData.resize(width * height * 3, 0);
        LeaveCriticalSection(&m_csFrameData);
        
        return S_OK;
    }

    STDMETHODIMP SetFramerate(DWORD fps) {
        if (fps == 0) {
            return E_INVALIDARG;
        }
        
        m_fps = fps;
        return S_OK;
    }

    // Get status information
    std::string GetStatus() const {
        char status[256];
        snprintf(status, sizeof(status), 
            "Custom Video Source Status:\n"
            "Resolution: %dx%d\n"
            "FPS: %d\n"
            "Frame Size: %zu bytes\n"
            "Running: Yes",
            m_width, m_height, m_fps, m_frameData.size());
        return std::string(status);
    }

private:
    LONG m_cRef;
    std::vector<BYTE> m_frameData;
    DWORD m_width;
    DWORD m_height;
    DWORD m_fps;
    CRITICAL_SECTION m_csFrameData;
};

// DLL entry point
BOOL APIENTRY DllMain(HMODULE hModule, DWORD ul_reason_for_call, LPVOID lpReserved)
{
    switch (ul_reason_for_call) {
    case DLL_PROCESS_ATTACH:
    case DLL_THREAD_ATTACH:
    case DLL_THREAD_DETACH:
    case DLL_PROCESS_DETACH:
        break;
    }
    return TRUE;
}

// Export functions for Rust integration
extern "C" {
    __declspec(dllexport) ICustomVideoSource* CreateCustomVideoSource() {
        return new CCustomVideoSourceFilter();
    }
    
    __declspec(dllexport) void DestroyCustomVideoSource(ICustomVideoSource* source) {
        if (source) {
            source->Release();
        }
    }
    
    __declspec(dllexport) HRESULT SetFrameData(ICustomVideoSource* source, const BYTE* data, DWORD size) {
        if (source) {
            return source->SetFrameData(data, size);
        }
        return E_INVALIDARG;
    }
    
    __declspec(dllexport) HRESULT SetResolution(ICustomVideoSource* source, DWORD width, DWORD height) {
        if (source) {
            return source->SetResolution(width, height);
        }
        return E_INVALIDARG;
    }
    
    __declspec(dllexport) HRESULT SetFramerate(ICustomVideoSource* source, DWORD fps) {
        if (source) {
            return source->SetFramerate(fps);
        }
        return E_INVALIDARG;
    }
    
    __declspec(dllexport) const char* GetStatus(ICustomVideoSource* source) {
        if (source) {
            // Note: This is a simplified version - in a real implementation,
            // you'd need to handle string memory management properly
            static std::string status;
            status = static_cast<CCustomVideoSourceFilter*>(source)->GetStatus();
            return status.c_str();
        }
        return "Custom Video Source not available";
    }
}

// Registration functions (simplified)
STDAPI DllRegisterServer()
{
    // In a real implementation, this would register the filter with the system
    std::cout << "CamLooper Custom Video Source Filter registered" << std::endl;
    return S_OK;
}

STDAPI DllUnregisterServer()
{
    // In a real implementation, this would unregister the filter
    std::cout << "CamLooper Custom Video Source Filter unregistered" << std::endl;
    return S_OK;
} 