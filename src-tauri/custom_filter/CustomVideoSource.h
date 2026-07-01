#pragma once

#include <windows.h>
#include <dshow.h>
#include <strmif.h>
#include <initguid.h>
#include <uuids.h>
#include <atlbase.h>
#include <atlcom.h>
#include <memory>
#include <vector>

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

// Custom video source filter class
class CCustomVideoSourceFilter : 
    public CBaseFilter,
    public ICustomVideoSource
{
public:
    DECLARE_IUNKNOWN;

    CCustomVideoSourceFilter(LPUNKNOWN pUnk, HRESULT* phr);
    virtual ~CCustomVideoSourceFilter();

    // IUnknown methods
    STDMETHODIMP QueryInterface(REFIID riid, void** ppv);
    STDMETHODIMP_(ULONG) AddRef();
    STDMETHODIMP_(ULONG) Release();

    // ICustomVideoSource methods
    STDMETHODIMP SetFrameData(const BYTE* data, DWORD size);
    STDMETHODIMP SetResolution(DWORD width, DWORD height);
    STDMETHODIMP SetFramerate(DWORD fps);

    // CBaseFilter methods
    virtual int GetPinCount();
    virtual CBasePin* GetPin(int n);

    // Filter registration
    static CUnknown* WINAPI CreateInstance(LPUNKNOWN pUnk, HRESULT* phr);

private:
    class CCustomVideoOutputPin;
    std::unique_ptr<CCustomVideoOutputPin> m_pOutputPin;
    
    // Frame data
    std::vector<BYTE> m_frameData;
    DWORD m_width;
    DWORD m_height;
    DWORD m_fps;
    CRITICAL_SECTION m_csFrameData;
    
    // Reference count
    LONG m_cRef;
};

// Output pin class
class CCustomVideoSourceFilter::CCustomVideoOutputPin : 
    public CBaseOutputPin
{
public:
    CCustomVideoOutputPin(CCustomVideoSourceFilter* pFilter, HRESULT* phr);
    virtual ~CCustomVideoOutputPin();

    // CBaseOutputPin methods
    virtual HRESULT GetMediaType(int iPosition, CMediaType* pMediaType);
    virtual HRESULT CheckMediaType(const CMediaType* pMediaType);
    virtual HRESULT DecideBufferSize(IMemAllocator* pAlloc, ALLOCATOR_PROPERTIES* pRequest);
    
    // Custom methods
    HRESULT DeliverFrame(const BYTE* data, DWORD size);

private:
    CCustomVideoSourceFilter* m_pFilter;
    IMemAllocator* m_pAllocator;
    IMediaSample* m_pSample;
}; 