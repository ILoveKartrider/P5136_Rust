#include <windows.h>
#include <dinput.h>

#include <cwchar>

#include "p5136_xun_api.h"

int wmain(int argc, wchar_t** argv) {
    if (argc != 3) {
        std::fwprintf(stderr, L"usage: p5136_xun_smoke <dinput8.dll> <P5136.exe>\n");
        return 2;
    }
    HMODULE module = LoadLibraryW(argv[1]);
    if (module == nullptr) {
        std::fwprintf(stderr, L"LoadLibrary failed: %lu\n", GetLastError());
        return 3;
    }
    using GetAbiVersion = uint32_t(P5136_XUN_CALL*)();
    using GetStatus = int32_t(P5136_XUN_CALL*)(P5136XunStatusSnapshot*);
    using VerifyExecutable = int32_t(P5136_XUN_CALL*)(const wchar_t*);
    using DirectInputCreate = HRESULT(WINAPI*)(HINSTANCE, DWORD, REFIID, LPVOID*, LPUNKNOWN);
    const auto get_abi = reinterpret_cast<GetAbiVersion>(GetProcAddress(module, "P5136XunGetAbiVersion"));
    const auto get_status = reinterpret_cast<GetStatus>(GetProcAddress(module, "P5136XunGetStatus"));
    const auto verify = reinterpret_cast<VerifyExecutable>(GetProcAddress(module, "P5136XunVerifyExecutableFile"));
    const auto direct_input = reinterpret_cast<DirectInputCreate>(GetProcAddress(module, "DirectInput8Create"));
    if (get_abi == nullptr || get_status == nullptr || verify == nullptr || direct_input == nullptr
        || get_abi() != P5136_XUN_ABI_VERSION) {
        std::fwprintf(stderr, L"sidecar diagnostic exports are missing\n");
        FreeLibrary(module);
        return 4;
    }
    if (verify(argv[2]) != 1 || verify(argv[0]) != 0) {
        std::fwprintf(stderr, L"exact executable verification failed\n");
        FreeLibrary(module);
        return 6;
    }
    void* input = nullptr;
    const HRESULT direct_input_result = direct_input(
        GetModuleHandleW(nullptr),
        DIRECTINPUT_VERSION,
        IID_IDirectInput8W,
        &input,
        nullptr);
    if (FAILED(direct_input_result) || input == nullptr) {
        std::fwprintf(stderr, L"DirectInput8Create forwarding failed: 0x%08lX\n", direct_input_result);
        FreeLibrary(module);
        return 7;
    }
    static_cast<IDirectInput8W*>(input)->Release();
    P5136XunStatusSnapshot snapshot{};
    snapshot.size = sizeof(snapshot);
    if (get_status(&snapshot) != 1
        || snapshot.status != P5136_XUN_STATUS_UNSUPPORTED_PROCESS
        || (snapshot.flags & P5136_XUN_FLAG_HOOKS_INSTALLED) != 0) {
        std::fwprintf(stderr, L"unexpected smoke status=%lu flags=0x%08lX\n", snapshot.status, snapshot.flags);
        FreeLibrary(module);
        return 5;
    }
    std::wprintf(
        L"abi=%lu status=%lu flags=0x%08lX timestamp=0x%08lX image_size=0x%08lX exact_file=1 dinput=ok\n",
        snapshot.abi_version,
        snapshot.status,
        snapshot.flags,
        snapshot.image_timestamp,
        snapshot.image_size);
    FreeLibrary(module);
    return 0;
}
