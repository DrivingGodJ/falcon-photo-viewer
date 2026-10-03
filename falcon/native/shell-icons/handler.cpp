// Legacy Falcon.Image defaults can cover several extensions. Windows keeps that
// choice protected; this helper supplies the right icon without changing it.
// It never opens the photo. Keep dependencies and work inside Explorer minimal.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <shlobj.h>
#include <shlwapi.h>
#include <strsafe.h>
#include <new>

#ifdef FALCON_ICON_TEST_CLASS
// Separate test-only build: native Shell activation can never collide with an
// installed production helper. This DLL is not embedded or distributed.
static const CLSID kClass = {0xd69608a2, 0x5041, 0x4666, {0x9f,0xa6,0x1a,0x0c,0x32,0x6f,0x58,0x7e}};
static const wchar_t kSettings[] = L"Software\\Classes\\CLSID\\{D69608A2-5041-4666-9FA6-1A0C326F587E}";
#else
static const CLSID kClass = {0xbbaba60e, 0x8fe6, 0x4b64, {0x8f,0x05,0xaa,0x30,0x6f,0x97,0x3b,0x4a}};
static const wchar_t kSettings[] = L"Software\\Classes\\CLSID\\{BBABA60E-8FE6-4B64-8F05-AA306F973B4A}";
#endif
static LONG objects = 0;
static LONG locks = 0;

// Fixed bounds, including registry strings: malformed input is a declined icon,
// never an unbounded allocation or a partial filename returned to Explorer.
static HRESULT ReadIcon(const wchar_t* key, const wchar_t* value,
                        wchar_t* output, UINT capacity, int* index) noexcept {
    wchar_t location[32768] = {};
    DWORD bytes = sizeof(location);
    const LSTATUS status = RegGetValueW(HKEY_CURRENT_USER, key, value,
        RRF_RT_REG_SZ, nullptr, location, &bytes);
    if (status != ERROR_SUCCESS) return S_FALSE;
    const int selected = PathParseIconLocationW(location);
    if (!location[0] || PathIsRelativeW(location) || selected >= 0) return S_FALSE;
    const HRESULT copied = StringCchCopyW(output, capacity, location);
    if (FAILED(copied)) return copied;
    *index = selected;
    return S_OK;
}

class Icon final : public IPersistFile, public IExtractIconW {
    LONG refs = 1;
    wchar_t extension[17] = {};
public:
    Icon() noexcept { InterlockedIncrement(&objects); }
    ~Icon() { InterlockedDecrement(&objects); }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (id == IID_IUnknown || id == IID_IPersist || id == IID_IPersistFile)
            *out = static_cast<IPersistFile*>(this);
        else if (id == IID_IExtractIconW) *out = static_cast<IExtractIconW*>(this);
        else return E_NOINTERFACE;
        AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs); }
    ULONG STDMETHODCALLTYPE Release() override {
        const LONG n = InterlockedDecrement(&refs);
        if (!n) delete this;
        return n;
    }
    HRESULT STDMETHODCALLTYPE GetClassID(CLSID* id) override {
        if (!id) return E_POINTER;
        *id = kClass; return S_OK;
    }
    HRESULT STDMETHODCALLTYPE IsDirty() override { return S_FALSE; }
    HRESULT STDMETHODCALLTYPE Load(LPCOLESTR path, DWORD) override {
        extension[0] = 0;
        if (!path) return E_POINTER;
        // Pure filename parsing, even for missing/offline/cloud-placeholder files.
        const wchar_t* dot = nullptr;
        size_t length = 0;
        for (; length < 32768 && path[length]; ++length) {
            if (path[length] == L'\\' || path[length] == L'/') dot = nullptr;
            else if (path[length] == L'.') dot = path + length + 1;
        }
        if (length == 32768) return E_INVALIDARG;
        if (!dot) return S_OK;
        const size_t count = (path + length) - dot;
        if (!count || count >= ARRAYSIZE(extension)) return S_OK;
        wchar_t candidate[17] = {};
        for (size_t i = 0; i < count; ++i) {
            wchar_t c = dot[i];
            if (c >= L'A' && c <= L'Z') c += L'a' - L'A';
            if (!((c >= L'a' && c <= L'z') || (c >= L'0' && c <= L'9'))) return S_OK;
            candidate[i] = c;
        }
        CopyMemory(extension, candidate, sizeof(extension));
        return S_OK;
    }
    HRESULT STDMETHODCALLTYPE Save(LPCOLESTR, BOOL) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE SaveCompleted(LPCOLESTR) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE GetCurFile(LPOLESTR* path) override {
        if (!path) return E_POINTER;
        *path = nullptr; return E_NOTIMPL;
    }
    HRESULT STDMETHODCALLTYPE GetIconLocation(UINT, LPWSTR path, UINT capacity,
                                               int* index, UINT* flags) override {
        if (!path || !index || !flags) return E_POINTER;
        *index = 0; *flags = 0;
        if (!capacity) return E_INVALIDARG;
        path[0] = 0;
        if (extension[0]) {
            wchar_t key[128];
            if (SUCCEEDED(StringCchPrintfW(key, ARRAYSIZE(key),
                    L"Software\\Classes\\Falcon.Image.%s\\DefaultIcon", extension))) {
                const HRESULT result = ReadIcon(key, nullptr, path, capacity, index);
                if (result != S_FALSE) return result;
            }
        }
        return ReadIcon(kSettings, L"FallbackIcon", path, capacity, index);
    }
    HRESULT STDMETHODCALLTYPE Extract(LPCWSTR, UINT, HICON* largeIcon, HICON* smallIcon, UINT) override {
        if (largeIcon) *largeIcon = nullptr;
        if (smallIcon) *smallIcon = nullptr;
        // Windows loads/caches the existing packaged icon returned above.
        return S_FALSE;
    }
};

class Factory final : public IClassFactory {
    LONG refs = 1;
public:
    Factory() noexcept { InterlockedIncrement(&objects); }
    ~Factory() { InterlockedDecrement(&objects); }
    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (id != IID_IUnknown && id != IID_IClassFactory) return E_NOINTERFACE;
        *out = static_cast<IClassFactory*>(this); AddRef(); return S_OK;
    }
    ULONG STDMETHODCALLTYPE AddRef() override { return InterlockedIncrement(&refs); }
    ULONG STDMETHODCALLTYPE Release() override {
        const LONG n = InterlockedDecrement(&refs);
        if (!n) delete this;
        return n;
    }
    HRESULT STDMETHODCALLTYPE CreateInstance(IUnknown* outer, REFIID id, void** out) override {
        if (!out) return E_POINTER;
        *out = nullptr;
        if (outer) return CLASS_E_NOAGGREGATION;
        auto* icon = new (std::nothrow) Icon;
        if (!icon) return E_OUTOFMEMORY;
        const HRESULT result = icon->QueryInterface(id, out);
        icon->Release(); return result;
    }
    HRESULT STDMETHODCALLTYPE LockServer(BOOL lock) override {
        if (lock) InterlockedIncrement(&locks); else InterlockedDecrement(&locks);
        return S_OK;
    }
};

STDAPI DllGetClassObject(REFCLSID id, REFIID iid, void** out) {
    if (!out) return E_POINTER;
    *out = nullptr;
    if (id != kClass) return CLASS_E_CLASSNOTAVAILABLE;
    auto* factory = new (std::nothrow) Factory;
    if (!factory) return E_OUTOFMEMORY;
    const HRESULT result = factory->QueryInterface(iid, out);
    factory->Release(); return result;
}

STDAPI DllCanUnloadNow() {
    return InterlockedCompareExchange(&objects, 0, 0) == 0 &&
           InterlockedCompareExchange(&locks, 0, 0) == 0 ? S_OK : S_FALSE;
}
