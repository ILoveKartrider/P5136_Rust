#include <windows.h>
#include <dinput.h>

#include "sidecar.hpp"

namespace {

INIT_ONCE g_proxy_once = INIT_ONCE_STATIC_INIT;
HMODULE g_real_dinput8 = nullptr;

struct ProxyExports {
    FARPROC direct_input8_create = nullptr;
    FARPROC dll_can_unload_now = nullptr;
    FARPROC dll_get_class_object = nullptr;
    FARPROC dll_register_server = nullptr;
    FARPROC dll_unregister_server = nullptr;
    FARPROC get_df_di_joystick = nullptr;
};

ProxyExports g_exports;

BOOL CALLBACK load_proxy(PINIT_ONCE, PVOID, PVOID*) noexcept {
    wchar_t path[MAX_PATH] = {};
    const UINT length = GetSystemDirectoryW(path, MAX_PATH);
    if (length == 0 || length + 13 >= MAX_PATH) {
        return TRUE;
    }
    lstrcatW(path, L"\\dinput8.dll");
    g_real_dinput8 = LoadLibraryW(path);
    if (g_real_dinput8 == nullptr) {
        return TRUE;
    }
    g_exports.direct_input8_create = GetProcAddress(g_real_dinput8, "DirectInput8Create");
    g_exports.dll_can_unload_now = GetProcAddress(g_real_dinput8, "DllCanUnloadNow");
    g_exports.dll_get_class_object = GetProcAddress(g_real_dinput8, "DllGetClassObject");
    g_exports.dll_register_server = GetProcAddress(g_real_dinput8, "DllRegisterServer");
    g_exports.dll_unregister_server = GetProcAddress(g_real_dinput8, "DllUnregisterServer");
    g_exports.get_df_di_joystick = GetProcAddress(g_real_dinput8, "GetdfDIJoystick");
    p5136_xun_set_proxy_ready(g_exports.direct_input8_create != nullptr);
    p5136_xun_initialize();
    return TRUE;
}

void ensure_proxy() noexcept {
    InitOnceExecuteOnce(&g_proxy_once, load_proxy, nullptr, nullptr);
}

template <typename Function>
Function export_as(FARPROC function) noexcept {
    return reinterpret_cast<Function>(function);
}

}  // namespace

extern "C" HRESULT WINAPI ProxyDirectInput8Create(
    HINSTANCE instance,
    DWORD version,
    REFIID interface_id,
    LPVOID* output,
    LPUNKNOWN outer) {
    ensure_proxy();
    using Function = HRESULT(WINAPI*)(HINSTANCE, DWORD, REFIID, LPVOID*, LPUNKNOWN);
    const Function function = export_as<Function>(g_exports.direct_input8_create);
    return function != nullptr
        ? function(instance, version, interface_id, output, outer)
        : HRESULT_FROM_WIN32(ERROR_MOD_NOT_FOUND);
}

extern "C" HRESULT WINAPI ProxyDllCanUnloadNow() {
    ensure_proxy();
    using Function = HRESULT(WINAPI*)();
    const Function function = export_as<Function>(g_exports.dll_can_unload_now);
    return function != nullptr ? function() : S_FALSE;
}

extern "C" HRESULT WINAPI ProxyDllGetClassObject(REFCLSID class_id, REFIID interface_id, LPVOID* output) {
    ensure_proxy();
    using Function = HRESULT(WINAPI*)(REFCLSID, REFIID, LPVOID*);
    const Function function = export_as<Function>(g_exports.dll_get_class_object);
    return function != nullptr ? function(class_id, interface_id, output) : CLASS_E_CLASSNOTAVAILABLE;
}

extern "C" HRESULT WINAPI ProxyDllRegisterServer() {
    ensure_proxy();
    using Function = HRESULT(WINAPI*)();
    const Function function = export_as<Function>(g_exports.dll_register_server);
    return function != nullptr ? function() : E_NOTIMPL;
}

extern "C" HRESULT WINAPI ProxyDllUnregisterServer() {
    ensure_proxy();
    using Function = HRESULT(WINAPI*)();
    const Function function = export_as<Function>(g_exports.dll_unregister_server);
    return function != nullptr ? function() : E_NOTIMPL;
}

extern "C" const DIDATAFORMAT* WINAPI ProxyGetdfDIJoystick() {
    ensure_proxy();
    using Function = const DIDATAFORMAT*(WINAPI*)();
    const Function function = export_as<Function>(g_exports.get_df_di_joystick);
    return function != nullptr ? function() : nullptr;
}

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        p5136_xun_set_module(instance);
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}
