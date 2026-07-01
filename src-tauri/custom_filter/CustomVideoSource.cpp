#include "CustomVideoSource.h"
#include <strmif.h>
#include <uuids.h>
#include <initguid.h>

// Filter registration
const AMOVIESETUP_MEDIATYPE sudPinTypes[] = {
    {
        &MEDIATYPE_Video,
        &MEDIASUBTYPE_RGB24
    }
};

const AMOVIESETUP_PIN sudPins[] = {
    {
        L"Output",
        FALSE,
        TRUE,
        FALSE,
        FALSE,
        &CLSID_NULL,
        NULL,
        1,
        sudPinTypes
    }
};

const AMOVIESETUP_FILTER sudFilter = {
    &CLSID_CustomVideoSource,
    L"CamLooper Custom Video Source",
    MERIT_DO_NOT_USE,
    1,
    sudPins
};

// DLL entry point
STDAPI DllRegisterServer()
{
    return AMovieDllRegisterServer2(TRUE);
}

STDAPI DllUnregisterServer()
{
    return AMovieDllRegisterServer2(FALSE);
}

// Filter factory
CFactoryTemplate g_Templates[] = {
    {
        L"CamLooper Custom Video Source",
        &CLSID_CustomVideoSource,
        CCustomVideoSourceFilter::CreateInstance,
        NULL,
        &sudFilter
    }
};

int g_cTemplates = sizeof(g_Templates) / sizeof(g_Templates[0]);

// CCustomVideoSourceFilter implementation
CCustomVideoSourceFilter::CCustomVideoSourceFilter(LPUNKNOWN pUnk, HRESULT* phr)
    : CBaseFilter(L"CamLooper Custom Video Source", pUnk, &m_cRef, CLSID_CustomVideoSource)
    , m_width(1920)
    , m_height(1080)
    , m_fps(30)
    , m_cRef(1)
{
    InitializeCriticalSection(&m_csFrameData);
    
    // Create output pin
    m_pOutputPin = std::make_unique<CCustomVideoOutputPin>(this, phr);
    if (FAILED(*phr)) {
        return;
    }
    
    // Initialize frame data
    m_frameData.resize(m_width * m_height * 3, 0);
}

CCustomVideoSourceFilter::~CCustomVideoSourceFilter()
{
    DeleteCriticalSection(&m_csFrameData);
}

// IUnknown methods
STDMETHODIMP CCustomVideoSourceFilter::QueryInterface(REFIID riid, void** ppv)
{
    if (riid == IID_IUnknown || riid == IID_ICustomVideoSource) {
        *ppv = static_cast<ICustomVideoSource*>(this);
        AddRef();
        return S_OK;
    }
    return CBaseFilter::QueryInterface(riid, ppv);
}

STDMETHODIMP_(ULONG) CCustomVideoSourceFilter::AddRef()
{
    return InterlockedIncrement(&m_cRef);
}

STDMETHODIMP_(ULONG) CCustomVideoSourceFilter::Release()
{
    LONG cRef = InterlockedDecrement(&m_cRef);
    if (cRef == 0) {
        delete this;
    }
    return cRef;
}

// ICustomVideoSource methods
STDMETHODIMP CCustomVideoSourceFilter::SetFrameData(const BYTE* data, DWORD size)
{
    if (!data || size == 0) {
        return E_INVALIDARG;
    }
    
    EnterCriticalSection(&m_csFrameData);
    
    if (size != m_width * m_height * 3) {
        LeaveCriticalSection(&m_csFrameData);
        return E_INVALIDARG;
    }
    
    m_frameData.assign(data, data + size);
    
    // Deliver frame to output pin
    if (m_pOutputPin) {
        m_pOutputPin->DeliverFrame(data, size);
    }
    
    LeaveCriticalSection(&m_csFrameData);
    return S_OK;
}

STDMETHODIMP CCustomVideoSourceFilter::SetResolution(DWORD width, DWORD height)
{
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

STDMETHODIMP CCustomVideoSourceFilter::SetFramerate(DWORD fps)
{
    if (fps == 0) {
        return E_INVALIDARG;
    }
    
    m_fps = fps;
    return S_OK;
}

// CBaseFilter methods
int CCustomVideoSourceFilter::GetPinCount()
{
    return 1;
}

CBasePin* CCustomVideoSourceFilter::GetPin(int n)
{
    if (n == 0) {
        return m_pOutputPin.get();
    }
    return NULL;
}

// Filter factory
CUnknown* WINAPI CCustomVideoSourceFilter::CreateInstance(LPUNKNOWN pUnk, HRESULT* phr)
{
    return new CCustomVideoSourceFilter(pUnk, phr);
}

// CCustomVideoOutputPin implementation
CCustomVideoSourceFilter::CCustomVideoOutputPin::CCustomVideoOutputPin(
    CCustomVideoSourceFilter* pFilter, HRESULT* phr)
    : CBaseOutputPin(L"Output", pFilter, pFilter, phr, L"Output")
    , m_pFilter(pFilter)
    , m_pAllocator(NULL)
    , m_pSample(NULL)
{
}

CCustomVideoSourceFilter::CCustomVideoOutputPin::~CCustomVideoOutputPin()
{
    if (m_pSample) {
        m_pSample->Release();
    }
    if (m_pAllocator) {
        m_pAllocator->Release();
    }
}

// CBaseOutputPin methods
HRESULT CCustomVideoSourceFilter::CCustomVideoOutputPin::GetMediaType(int iPosition, CMediaType* pMediaType)
{
    if (iPosition < 0) {
        return E_INVALIDARG;
    }
    
    if (iPosition > 0) {
        return VFW_S_NO_MORE_ITEMS;
    }
    
    // Set up video media type
    pMediaType->SetType(&MEDIATYPE_Video);
    pMediaType->SetSubtype(&MEDIASUBTYPE_RGB24);
    pMediaType->SetFormatType(&FORMAT_VideoInfo);
    
    // Set up video info header
    VIDEOINFOHEADER vih;
    ZeroMemory(&vih, sizeof(vih));
    
    vih.bmiHeader.biSize = sizeof(BITMAPINFOHEADER);
    vih.bmiHeader.biWidth = m_pFilter->m_width;
    vih.bmiHeader.biHeight = m_pFilter->m_height;
    vih.bmiHeader.biPlanes = 1;
    vih.bmiHeader.biBitCount = 24;
    vih.bmiHeader.biCompression = BI_RGB;
    vih.bmiHeader.biSizeImage = m_pFilter->m_width * m_pFilter->m_height * 3;
    
    vih.AvgTimePerFrame = 10000000 / m_pFilter->m_fps; // 100ns units
    
    pMediaType->SetFormat((BYTE*)&vih, sizeof(vih));
    
    return S_OK;
}

HRESULT CCustomVideoSourceFilter::CCustomVideoOutputPin::CheckMediaType(const CMediaType* pMediaType)
{
    if (pMediaType->majortype != MEDIATYPE_Video) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    
    if (pMediaType->subtype != MEDIASUBTYPE_RGB24) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    
    if (pMediaType->formattype != FORMAT_VideoInfo) {
        return VFW_E_TYPE_NOT_ACCEPTED;
    }
    
    return S_OK;
}

HRESULT CCustomVideoSourceFilter::CCustomVideoOutputPin::DecideBufferSize(
    IMemAllocator* pAlloc, ALLOCATOR_PROPERTIES* pRequest)
{
    ALLOCATOR_PROPERTIES props;
    
    props.cBuffers = 3;
    props.cbBuffer = m_pFilter->m_width * m_pFilter->m_height * 3;
    props.cbAlign = 1;
    props.cbPrefix = 0;
    
    ALLOCATOR_PROPERTIES actual;
    HRESULT hr = pAlloc->SetProperties(&props, &actual);
    if (FAILED(hr)) {
        return hr;
    }
    
    if (actual.cbBuffer < props.cbBuffer) {
        return E_FAIL;
    }
    
    return S_OK;
}

// Custom methods
HRESULT CCustomVideoSourceFilter::CCustomVideoOutputPin::DeliverFrame(const BYTE* data, DWORD size)
{
    if (!IsConnected()) {
        return S_OK;
    }
    
    // Get allocator if not already obtained
    if (!m_pAllocator) {
        IMemAllocator* pAlloc = NULL;
        HRESULT hr = GetConnected()->QueryInterface(IID_IMemAllocator, (void**)&pAlloc);
        if (FAILED(hr)) {
            return hr;
        }
        m_pAllocator = pAlloc;
    }
    
    // Get media sample
    if (!m_pSample) {
        IMediaSample* pSample = NULL;
        HRESULT hr = m_pAllocator->GetBuffer(&pSample, NULL, NULL, 0);
        if (FAILED(hr)) {
            return hr;
        }
        m_pSample = pSample;
    }
    
    // Copy frame data
    BYTE* pData = NULL;
    HRESULT hr = m_pSample->GetPointer(&pData);
    if (FAILED(hr)) {
        return hr;
    }
    
    memcpy(pData, data, size);
    m_pSample->SetActualDataLength(size);
    
    // Set timestamp
    REFERENCE_TIME start = 0;
    REFERENCE_TIME stop = 10000000 / m_pFilter->m_fps; // 100ns units
    m_pSample->SetTime(&start, &stop);
    
    // Deliver sample
    hr = Deliver(m_pSample);
    
    // Release sample for reuse
    m_pSample->Release();
    m_pSample = NULL;
    
    return hr;
} 