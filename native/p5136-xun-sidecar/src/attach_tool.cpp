#include <windows.h>
#include <tlhelp32.h>

#include <cstdint>
#include <cstdio>
#include <cwchar>
#include <string>
#include <vector>

#include "p5136_xun_api.h"

namespace {

constexpr wchar_t kDefaultProcessName[] = L"KartRider.exe";
constexpr wchar_t kAlternateProcessName[] = L"KartRiderU.exe";
constexpr wchar_t kDefaultDllName[] = L"p5136-xun.dll";

class ScopedHandle {
public:
    explicit ScopedHandle(HANDLE handle = nullptr) noexcept : handle_(handle) {}
    ~ScopedHandle() {
        if (valid()) {
            CloseHandle(handle_);
        }
    }
    ScopedHandle(const ScopedHandle&) = delete;
    ScopedHandle& operator=(const ScopedHandle&) = delete;
    [[nodiscard]] bool valid() const noexcept {
        return handle_ != nullptr && handle_ != INVALID_HANDLE_VALUE;
    }
    [[nodiscard]] HANDLE get() const noexcept {
        return handle_;
    }

private:
    HANDLE handle_;
};

int fail_last_error(const wchar_t* operation, int code) {
    std::fwprintf(stderr, L"%ls failed (Win32=%lu)\n", operation, GetLastError());
    return code;
}

std::wstring full_path(const wchar_t* path) {
    std::vector<wchar_t> buffer(32768);
    const DWORD length = GetFullPathNameW(path, static_cast<DWORD>(buffer.size()), buffer.data(), nullptr);
    if (length == 0 || length >= buffer.size()) {
        return {};
    }
    return std::wstring(buffer.data(), length);
}

std::wstring sibling_path(const wchar_t* name) {
    std::vector<wchar_t> buffer(32768);
    const DWORD length = GetModuleFileNameW(nullptr, buffer.data(), static_cast<DWORD>(buffer.size()));
    if (length == 0 || length >= buffer.size()) {
        return {};
    }
    std::wstring path(buffer.data(), length);
    const std::wstring::size_type separator = path.find_last_of(L"\\/");
    if (separator == std::wstring::npos) {
        return std::wstring(name);
    }
    path.resize(separator + 1);
    path.append(name);
    return path;
}

std::vector<DWORD> find_default_processes() {
    std::vector<DWORD> result;
    ScopedHandle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0));
    if (!snapshot.valid()) {
        return result;
    }
    PROCESSENTRY32W entry{};
    entry.dwSize = sizeof(entry);
    if (Process32FirstW(snapshot.get(), &entry) == FALSE) {
        return result;
    }
    do {
        if (_wcsicmp(entry.szExeFile, kDefaultProcessName) == 0
            || _wcsicmp(entry.szExeFile, kAlternateProcessName) == 0) {
            result.push_back(entry.th32ProcessID);
        }
    } while (Process32NextW(snapshot.get(), &entry) != FALSE);
    return result;
}

bool parse_pid(const wchar_t* text, DWORD* output) {
    if (text == nullptr || text[0] == L'\0') {
        return false;
    }
    wchar_t* end = nullptr;
    const unsigned long value = std::wcstoul(text, &end, 10);
    if (end == text || *end != L'\0' || value == 0 || value > MAXDWORD) {
        return false;
    }
    *output = static_cast<DWORD>(value);
    return true;
}

std::wstring process_image_path(HANDLE process) {
    std::vector<wchar_t> buffer(32768);
    DWORD length = static_cast<DWORD>(buffer.size());
    if (QueryFullProcessImageNameW(process, 0, buffer.data(), &length) == FALSE) {
        return {};
    }
    return std::wstring(buffer.data(), length);
}

std::uintptr_t remote_module_base(DWORD process_id, const wchar_t* module_name) {
    ScopedHandle snapshot(CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, process_id));
    if (!snapshot.valid()) {
        return 0;
    }
    MODULEENTRY32W entry{};
    entry.dwSize = sizeof(entry);
    if (Module32FirstW(snapshot.get(), &entry) == FALSE) {
        return 0;
    }
    do {
        if (_wcsicmp(entry.szModule, module_name) == 0) {
            return reinterpret_cast<std::uintptr_t>(entry.modBaseAddr);
        }
    } while (Module32NextW(snapshot.get(), &entry) != FALSE);
    return 0;
}

std::uintptr_t remote_system_export(DWORD process_id, const char* export_name) {
    const HMODULE local_kernel = GetModuleHandleW(L"kernel32.dll");
    if (local_kernel == nullptr) {
        return 0;
    }
    const FARPROC local_export = GetProcAddress(local_kernel, export_name);
    if (local_export == nullptr) {
        return 0;
    }

    MEMORY_BASIC_INFORMATION memory{};
    const auto export_address = reinterpret_cast<const void*>(reinterpret_cast<std::uintptr_t>(local_export));
    if (VirtualQuery(export_address, &memory, sizeof(memory)) != sizeof(memory)
        || memory.AllocationBase == nullptr) {
        return 0;
    }
    const auto owner = static_cast<HMODULE>(memory.AllocationBase);
    wchar_t owner_path[MAX_PATH] = {};
    const DWORD owner_length = GetModuleFileNameW(owner, owner_path, MAX_PATH);
    if (owner_length == 0 || owner_length >= MAX_PATH) {
        return 0;
    }
    const wchar_t* owner_name = owner_path;
    for (DWORD index = 0; index < owner_length; ++index) {
        if (owner_path[index] == L'\\' || owner_path[index] == L'/') {
            owner_name = owner_path + index + 1;
        }
    }
    const std::uintptr_t remote_owner = remote_module_base(process_id, owner_name);
    if (remote_owner == 0) {
        return 0;
    }
    const std::uintptr_t export_offset = reinterpret_cast<std::uintptr_t>(local_export)
        - reinterpret_cast<std::uintptr_t>(owner);
    return remote_owner + export_offset;
}

std::uintptr_t exported_rva(const std::wstring& dll_path, const char* export_name) {
    const HMODULE image = LoadLibraryExW(dll_path.c_str(), nullptr, DONT_RESOLVE_DLL_REFERENCES);
    if (image == nullptr) {
        return 0;
    }
    const FARPROC function = GetProcAddress(image, export_name);
    const std::uintptr_t result = function == nullptr
        ? 0
        : reinterpret_cast<std::uintptr_t>(function) - reinterpret_cast<std::uintptr_t>(image);
    FreeLibrary(image);
    return result;
}

const wchar_t* file_name(const std::wstring& path) {
    const std::wstring::size_type separator = path.find_last_of(L"\\/");
    return separator == std::wstring::npos ? path.c_str() : path.c_str() + separator + 1;
}

bool wait_for_thread(HANDLE thread, DWORD* result) {
    if (WaitForSingleObject(thread, 15000) != WAIT_OBJECT_0) {
        return false;
    }
    return GetExitCodeThread(thread, result) != FALSE;
}

int attach(DWORD process_id, const std::wstring& dll_path) {
    constexpr DWORD access = PROCESS_CREATE_THREAD | PROCESS_QUERY_INFORMATION
        | PROCESS_VM_OPERATION | PROCESS_VM_WRITE | PROCESS_VM_READ;
    ScopedHandle process(OpenProcess(access, FALSE, process_id));
    if (!process.valid()) {
        const DWORD error = GetLastError();
        if (error == ERROR_INVALID_PARAMETER) {
            std::fwprintf(stderr, L"target pid=%lu no longer exists; let the helper rediscover it\n", process_id);
        } else if (error == ERROR_ACCESS_DENIED) {
            std::fwprintf(stderr, L"OpenProcess denied; run this tool at the same elevation as the client\n");
        } else {
            std::fwprintf(stderr, L"OpenProcess failed for pid=%lu (Win32=%lu)\n", process_id, error);
        }
        return 10;
    }

    const std::wstring image_path = process_image_path(process.get());
    if (image_path.empty()) {
        return fail_last_error(L"QueryFullProcessImageNameW", 11);
    }
    if (P5136XunVerifyExecutableFile(image_path.c_str()) != 1) {
        std::fwprintf(stderr, L"refusing unsupported process image: %ls\n", image_path.c_str());
        return 12;
    }
    if (GetFileAttributesW(dll_path.c_str()) == INVALID_FILE_ATTRIBUTES) {
        return fail_last_error(L"locating p5136-xun.dll", 13);
    }

    const wchar_t* dll_name = file_name(dll_path);
    std::uintptr_t remote_dll = remote_module_base(process_id, dll_name);
    bool loaded_now = false;
    if (remote_dll == 0) {
        const std::uintptr_t remote_load_library = remote_system_export(process_id, "LoadLibraryW");
        if (remote_load_library == 0) {
            std::fwprintf(stderr, L"could not resolve remote LoadLibraryW\n");
            return 14;
        }
        const SIZE_T path_bytes = (dll_path.size() + 1) * sizeof(wchar_t);
        void* remote_path = VirtualAllocEx(
            process.get(), nullptr, path_bytes, MEM_COMMIT | MEM_RESERVE, PAGE_READWRITE);
        if (remote_path == nullptr) {
            return fail_last_error(L"VirtualAllocEx", 15);
        }
        SIZE_T written = 0;
        const bool wrote_path = WriteProcessMemory(
            process.get(), remote_path, dll_path.c_str(), path_bytes, &written) != FALSE
            && written == path_bytes;
        if (!wrote_path) {
            VirtualFreeEx(process.get(), remote_path, 0, MEM_RELEASE);
            return fail_last_error(L"WriteProcessMemory", 16);
        }
        ScopedHandle load_thread(CreateRemoteThread(
            process.get(),
            nullptr,
            0,
            reinterpret_cast<LPTHREAD_START_ROUTINE>(remote_load_library),
            remote_path,
            0,
            nullptr));
        if (!load_thread.valid()) {
            VirtualFreeEx(process.get(), remote_path, 0, MEM_RELEASE);
            return fail_last_error(L"CreateRemoteThread(LoadLibraryW)", 17);
        }
        DWORD load_result = 0;
        const bool loaded = wait_for_thread(load_thread.get(), &load_result);
        VirtualFreeEx(process.get(), remote_path, 0, MEM_RELEASE);
        if (!loaded || load_result == 0) {
            std::fwprintf(stderr, L"remote LoadLibraryW failed or timed out\n");
            return 18;
        }
        remote_dll = static_cast<std::uintptr_t>(load_result);
        loaded_now = true;
    }

    const std::uintptr_t initialize_rva = exported_rva(dll_path, "P5136XunInitialize");
    if (initialize_rva == 0) {
        std::fwprintf(stderr, L"P5136XunInitialize export is missing\n");
        return 19;
    }
    ScopedHandle initialize_thread(CreateRemoteThread(
        process.get(),
        nullptr,
        0,
        reinterpret_cast<LPTHREAD_START_ROUTINE>(remote_dll + initialize_rva),
        nullptr,
        0,
        nullptr));
    if (!initialize_thread.valid()) {
        return fail_last_error(L"CreateRemoteThread(P5136XunInitialize)", 20);
    }
    DWORD status = 0;
    if (!wait_for_thread(initialize_thread.get(), &status)) {
        std::fwprintf(stderr, L"remote initialization failed or timed out\n");
        return 21;
    }
    if (status != P5136_XUN_STATUS_EXACT_BUILD_DISABLED
        && status != P5136_XUN_STATUS_DIAGNOSTIC_READY
        && status != P5136_XUN_STATUS_LIFECYCLE_PROBE_READY
        && status != P5136_XUN_STATUS_XUN_TACHO_ALIAS_READY) {
        std::fwprintf(stderr, L"sidecar rejected initialization with status=%lu\n", status);
        return 22;
    }
    const wchar_t* hooks = status == P5136_XUN_STATUS_XUN_TACHO_ALIAS_READY
        ? L"xun-tacho+physics-state+six-consumers+charger-visual"
        : status == P5136_XUN_STATUS_LIFECYCLE_PROBE_READY
        ? L"lifecycle+physics-state+six-consumers+charger-visual"
        : L"not-installed";
    std::wprintf(
        L"attached pid=%lu image=%ls dll=%ls loaded_now=%d status=%lu hooks=%ls\n",
        process_id,
        image_path.c_str(),
        dll_path.c_str(),
        loaded_now ? 1 : 0,
        status,
        hooks);
    return 0;
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc > 3) {
        std::fwprintf(stderr, L"usage: p5136-xun-attach.exe [pid] [dll] | [dll]\n");
        return 2;
    }

    DWORD process_id = 0;
    std::wstring requested_dll;
    bool discover_process = argc == 1;
    if (argc == 2) {
        if (!parse_pid(argv[1], &process_id)) {
            requested_dll = full_path(argv[1]);
            discover_process = true;
        }
    } else if (argc == 3) {
        if (!parse_pid(argv[1], &process_id)) {
            std::fwprintf(stderr, L"invalid process id: %ls\n", argv[1]);
            return 3;
        }
        requested_dll = full_path(argv[2]);
    }

    if (discover_process) {
        const std::vector<DWORD> candidates = find_default_processes();
        if (candidates.size() != 1) {
            std::fwprintf(stderr, L"expected one KartRider process, found %zu; pass its pid explicitly\n", candidates.size());
            for (const DWORD candidate : candidates) {
                std::fwprintf(stderr, L"  pid=%lu\n", candidate);
            }
            return 4;
        }
        process_id = candidates.front();
    }

    const std::wstring dll_path = requested_dll.empty()
        ? full_path(sibling_path(kDefaultDllName).c_str())
        : requested_dll;
    if (dll_path.empty()) {
        return fail_last_error(L"GetFullPathNameW", 5);
    }
    return attach(process_id, dll_path);
}
