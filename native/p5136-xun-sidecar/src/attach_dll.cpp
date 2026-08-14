#include <windows.h>

#include "sidecar.hpp"

BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID) {
    if (reason == DLL_PROCESS_ATTACH) {
        p5136_xun_set_module(instance);
        DisableThreadLibraryCalls(instance);
    }
    return TRUE;
}
