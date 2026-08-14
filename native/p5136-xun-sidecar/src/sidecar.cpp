#include "sidecar.hpp"

#include "lifecycle_hooks.hpp"
#include "xun_physics_runtime.hpp"
#include "xun_profile_client.hpp"

#include <wincrypt.h>

#include <array>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <cwchar>

namespace {

constexpr DWORD kExpectedTimestamp = 0x6407DD11u;
constexpr DWORD kExpectedImageSize = 0x0141A000u;
constexpr DWORD kExpectedEntryPoint = 0x00BE4D56u;
constexpr std::array<BYTE, 32> kExpectedSha256 = {
    0xFD, 0x94, 0x44, 0xC0, 0x57, 0x09, 0x0C, 0x3B,
    0xB5, 0x24, 0xAF, 0x03, 0xBF, 0xF5, 0xEC, 0x99,
    0x56, 0x20, 0xFB, 0xB9, 0x51, 0xB9, 0xA8, 0x23,
    0xD2, 0xCD, 0x4E, 0x9B, 0x04, 0x94, 0x95, 0x6F,
};
constexpr std::size_t kLogQueueCapacity = 256;
constexpr std::size_t kLogMessageCharacters = 640;

INIT_ONCE g_initialize_once = INIT_ONCE_STATIC_INIT;
SRWLOCK g_state_lock = SRWLOCK_INIT;
HMODULE g_module = nullptr;
P5136XunStatusSnapshot g_state = {
    sizeof(P5136XunStatusSnapshot),
    P5136_XUN_ABI_VERSION,
    P5136_XUN_STATUS_UNINITIALIZED,
    0,
    0,
    0,
    0,
    0,
    {},
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
    0,
};

struct QueuedLogEntry {
    SYSTEMTIME timestamp{};
    DWORD process_id = 0;
    wchar_t message[kLogMessageCharacters]{};
};

SRWLOCK g_log_lock = SRWLOCK_INIT;
std::array<QueuedLogEntry, kLogQueueCapacity> g_log_queue{};
std::size_t g_log_read_index = 0;
std::size_t g_log_write_index = 0;
std::size_t g_log_count = 0;
std::uint32_t g_log_dropped = 0;
HANDLE g_log_event = nullptr;
volatile LONG g_log_ready = 0;

struct ImageIdentity {
    DWORD timestamp = 0;
    DWORD image_size = 0;
    DWORD entry_point = 0;
    std::array<BYTE, 32> sha256{};
    bool pe_valid = false;
    bool hash_valid = false;
};

bool append_path(wchar_t* path, DWORD capacity, const wchar_t* suffix) noexcept {
    const DWORD length = static_cast<DWORD>(lstrlenW(path));
    const DWORD suffix_length = static_cast<DWORD>(lstrlenW(suffix));
    if (length + suffix_length + 1 > capacity) {
        return false;
    }
    std::memcpy(path + length, suffix, (suffix_length + 1) * sizeof(wchar_t));
    return true;
}

bool module_directory(wchar_t* output, DWORD capacity) noexcept {
    if (g_module == nullptr || capacity == 0) {
        return false;
    }
    const DWORD length = GetModuleFileNameW(g_module, output, capacity);
    if (length == 0 || length >= capacity) {
        return false;
    }
    for (DWORD index = length; index > 0; --index) {
        if (output[index - 1] == L'\\' || output[index - 1] == L'/') {
            output[index] = L'\0';
            return true;
        }
    }
    return false;
}

bool write_log_entry(HANDLE file, const QueuedLogEntry& entry) noexcept {
    wchar_t line[1024] = {};
    const int characters = wsprintfW(
        line,
        L"[%04u-%02u-%02u %02u:%02u:%02u.%03u] pid=%lu %s\r\n",
        entry.timestamp.wYear,
        entry.timestamp.wMonth,
        entry.timestamp.wDay,
        entry.timestamp.wHour,
        entry.timestamp.wMinute,
        entry.timestamp.wSecond,
        entry.timestamp.wMilliseconds,
        entry.process_id,
        entry.message);
    if (characters <= 0) {
        return false;
    }
    char utf8[2048] = {};
    const int bytes = WideCharToMultiByte(
        CP_UTF8,
        WC_ERR_INVALID_CHARS,
        line,
        characters,
        utf8,
        static_cast<int>(sizeof(utf8)),
        nullptr,
        nullptr);
    if (bytes <= 0) {
        return false;
    }
    DWORD written = 0;
    return WriteFile(file, utf8, static_cast<DWORD>(bytes), &written, nullptr) != FALSE
        && written == static_cast<DWORD>(bytes);
}

bool dequeue_log(QueuedLogEntry* output, std::uint32_t* dropped) noexcept {
    bool available = false;
    AcquireSRWLockExclusive(&g_log_lock);
    if (g_log_count != 0) {
        *output = g_log_queue[g_log_read_index];
        g_log_read_index = (g_log_read_index + 1u) % kLogQueueCapacity;
        --g_log_count;
        available = true;
        *dropped = 0;
    } else {
        *dropped = g_log_dropped;
        g_log_dropped = 0;
    }
    ReleaseSRWLockExclusive(&g_log_lock);
    return available;
}

DWORD WINAPI log_writer_thread(void*) noexcept {
    wchar_t path[MAX_PATH] = {};
    if (!module_directory(path, MAX_PATH) || !append_path(path, MAX_PATH, L"p5136-xun-sidecar.log")) {
        InterlockedExchange(&g_log_ready, 0);
        return 0;
    }
    HANDLE file = CreateFileW(
        path,
        FILE_APPEND_DATA,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        nullptr,
        OPEN_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        InterlockedExchange(&g_log_ready, 0);
        return 0;
    }
    DWORD last_flush_ms = GetTickCount();
    for (;;) {
        WaitForSingleObject(g_log_event, 500);
        bool wrote = false;
        for (;;) {
            QueuedLogEntry entry{};
            std::uint32_t dropped = 0;
            if (dequeue_log(&entry, &dropped)) {
                wrote = write_log_entry(file, entry) || wrote;
                continue;
            }
            if (dropped != 0) {
                GetLocalTime(&entry.timestamp);
                entry.process_id = GetCurrentProcessId();
                wsprintfW(
                    entry.message,
                    L"async logger: dropped %lu entries because the fixed queue was full",
                    dropped);
                wrote = write_log_entry(file, entry) || wrote;
            }
            break;
        }
        const DWORD now_ms = GetTickCount();
        if (wrote && now_ms - last_flush_ms >= 1000u) {
            FlushFileBuffers(file);
            last_flush_ms = now_ms;
        }
    }
}

bool start_async_logger() noexcept {
    if (InterlockedCompareExchange(&g_log_ready, 0, 0) != 0) {
        return true;
    }
    HMODULE pinned_module = nullptr;
    if (GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_PIN | GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
            reinterpret_cast<LPCWSTR>(g_log_queue.data()),
            &pinned_module) == FALSE) {
        return false;
    }
    g_log_event = CreateEventW(nullptr, FALSE, FALSE, nullptr);
    if (g_log_event == nullptr) {
        return false;
    }
    InterlockedExchange(&g_log_ready, 1);
    HANDLE thread = CreateThread(nullptr, 0, log_writer_thread, nullptr, 0, nullptr);
    if (thread == nullptr) {
        InterlockedExchange(&g_log_ready, 0);
        CloseHandle(g_log_event);
        g_log_event = nullptr;
        return false;
    }
    SetThreadPriority(thread, THREAD_PRIORITY_BELOW_NORMAL);
    CloseHandle(thread);
    return true;
}

void append_log(const wchar_t* message) noexcept {
    if (message == nullptr
        || InterlockedCompareExchange(&g_log_ready, 0, 0) == 0
        || g_log_event == nullptr) {
        return;
    }
    QueuedLogEntry entry{};
    GetLocalTime(&entry.timestamp);
    entry.process_id = GetCurrentProcessId();
    lstrcpynW(entry.message, message, static_cast<int>(kLogMessageCharacters));

    bool queued = false;
    bool wake_writer = false;
    AcquireSRWLockExclusive(&g_log_lock);
    if (g_log_count < kLogQueueCapacity) {
        wake_writer = g_log_count == 0;
        g_log_queue[g_log_write_index] = entry;
        g_log_write_index = (g_log_write_index + 1u) % kLogQueueCapacity;
        ++g_log_count;
        queued = true;
    } else {
        ++g_log_dropped;
    }
    ReleaseSRWLockExclusive(&g_log_lock);
    if (queued && wake_writer) {
        SetEvent(g_log_event);
    }
}

bool sha256_file(const wchar_t* path, std::array<BYTE, 32>* output) noexcept {
    HANDLE file = CreateFileW(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN,
        nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return false;
    }
    HCRYPTPROV provider = 0;
    HCRYPTHASH hash = 0;
    bool success = CryptAcquireContextW(&provider, nullptr, nullptr, PROV_RSA_AES, CRYPT_VERIFYCONTEXT) != FALSE
        && CryptCreateHash(provider, CALG_SHA_256, 0, 0, &hash) != FALSE;
    std::array<BYTE, 64 * 1024> buffer{};
    while (success) {
        DWORD read = 0;
        if (!ReadFile(file, buffer.data(), static_cast<DWORD>(buffer.size()), &read, nullptr)) {
            success = false;
            break;
        }
        if (read == 0) {
            break;
        }
        success = CryptHashData(hash, buffer.data(), read, 0) != FALSE;
    }
    if (success) {
        DWORD bytes = static_cast<DWORD>(output->size());
        success = CryptGetHashParam(hash, HP_HASHVAL, output->data(), &bytes, 0) != FALSE
            && bytes == output->size();
    }
    if (hash != 0) {
        CryptDestroyHash(hash);
    }
    if (provider != 0) {
        CryptReleaseContext(provider, 0);
    }
    CloseHandle(file);
    return success;
}

ImageIdentity read_image_identity() noexcept {
    ImageIdentity identity;
    const auto* base = reinterpret_cast<const BYTE*>(GetModuleHandleW(nullptr));
    if (base == nullptr) {
        return identity;
    }
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(base);
    if (dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew <= 0) {
        return identity;
    }
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS32*>(base + dos->e_lfanew);
    if (nt->Signature != IMAGE_NT_SIGNATURE
        || nt->OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR32_MAGIC
        || nt->FileHeader.Machine != IMAGE_FILE_MACHINE_I386) {
        return identity;
    }
    identity.timestamp = nt->FileHeader.TimeDateStamp;
    identity.image_size = nt->OptionalHeader.SizeOfImage;
    identity.entry_point = nt->OptionalHeader.AddressOfEntryPoint;
    identity.pe_valid = true;

    wchar_t executable[MAX_PATH] = {};
    const DWORD length = GetModuleFileNameW(nullptr, executable, MAX_PATH);
    if (length != 0 && length < MAX_PATH) {
        identity.hash_valid = sha256_file(executable, &identity.sha256);
    }
    return identity;
}

ImageIdentity read_file_identity(const wchar_t* path) noexcept {
    ImageIdentity identity;
    if (path == nullptr || path[0] == L'\0') {
        return identity;
    }
    HANDLE file = CreateFileW(
        path,
        GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        nullptr,
        OPEN_EXISTING,
        FILE_ATTRIBUTE_NORMAL | FILE_FLAG_SEQUENTIAL_SCAN,
        nullptr);
    if (file == INVALID_HANDLE_VALUE) {
        return identity;
    }
    IMAGE_DOS_HEADER dos{};
    DWORD read = 0;
    bool valid = ReadFile(file, &dos, sizeof(dos), &read, nullptr) != FALSE
        && read == sizeof(dos)
        && dos.e_magic == IMAGE_DOS_SIGNATURE
        && dos.e_lfanew > 0;
    LARGE_INTEGER offset{};
    offset.QuadPart = dos.e_lfanew;
    IMAGE_NT_HEADERS32 nt{};
    valid = valid
        && SetFilePointerEx(file, offset, nullptr, FILE_BEGIN) != FALSE
        && ReadFile(file, &nt, sizeof(nt), &read, nullptr) != FALSE
        && read == sizeof(nt)
        && nt.Signature == IMAGE_NT_SIGNATURE
        && nt.OptionalHeader.Magic == IMAGE_NT_OPTIONAL_HDR32_MAGIC
        && nt.FileHeader.Machine == IMAGE_FILE_MACHINE_I386;
    CloseHandle(file);
    if (!valid) {
        return identity;
    }
    identity.timestamp = nt.FileHeader.TimeDateStamp;
    identity.image_size = nt.OptionalHeader.SizeOfImage;
    identity.entry_point = nt.OptionalHeader.AddressOfEntryPoint;
    identity.pe_valid = true;
    identity.hash_valid = sha256_file(path, &identity.sha256);
    return identity;
}

bool is_exact_build(const ImageIdentity& identity) noexcept {
    return identity.pe_valid
        && identity.hash_valid
        && identity.timestamp == kExpectedTimestamp
        && identity.image_size == kExpectedImageSize
        && identity.entry_point == kExpectedEntryPoint
        && identity.sha256 == kExpectedSha256;
}

bool config_enabled() noexcept {
    wchar_t path[MAX_PATH] = {};
    if (!module_directory(path, MAX_PATH) || !append_path(path, MAX_PATH, L"p5136-xun.ini")) {
        return false;
    }
    return GetPrivateProfileIntW(L"xun", L"enabled", 0, path) == 1;
}

bool config_logging_enabled() noexcept {
    wchar_t path[MAX_PATH] = {};
    if (!module_directory(path, MAX_PATH) || !append_path(path, MAX_PATH, L"p5136-xun.ini")) {
        return true;
    }
    return GetPrivateProfileIntW(L"xun", L"logging", 1, path) == 1;
}

BOOL CALLBACK initialize_once(PINIT_ONCE, PVOID, PVOID*) noexcept {
    if (config_logging_enabled()) {
        static_cast<void>(start_async_logger());
    }
    const ImageIdentity identity = read_image_identity();
    const bool exact_build = is_exact_build(identity);
    const bool enabled = config_enabled();
    p5136::xun::ChargerTuning physics_tuning{};
    // Hooks are installed up front, but every consumer remains fail-closed
    // until the authenticated server-side profile channel selects a supported
    // speed XUN kart for this nickname.
    p5136_xun_configure_physics_probe(physics_tuning);
    wchar_t hook_error[256] = {};
    const bool hooks_installed = exact_build
        && enabled
        && p5136_xun_install_lifecycle_hooks(hook_error, _countof(hook_error));
    if (hooks_installed) {
        p5136_xun_start_profile_client();
    }

    AcquireSRWLockExclusive(&g_state_lock);
    g_state.process_id = GetCurrentProcessId();
    g_state.image_timestamp = identity.timestamp;
    g_state.image_size = identity.image_size;
    g_state.entry_point_rva = identity.entry_point;
    std::memcpy(g_state.image_sha256, identity.sha256.data(), identity.sha256.size());
    if (exact_build) {
        g_state.flags |= P5136_XUN_FLAG_EXACT_P5136_BUILD;
    }
    if (enabled) {
        g_state.flags |= P5136_XUN_FLAG_CONFIG_ENABLED;
    }
    if (hooks_installed) {
        g_state.flags |= P5136_XUN_FLAG_HOOKS_INSTALLED;
        g_state.flags |= P5136_XUN_FLAG_XUN_TACHO_ALIAS_REGISTERED;
        g_state.flags |= P5136_XUN_FLAG_PHYSICS_STATE_PROBE_INSTALLED;
        g_state.flags |= P5136_XUN_FLAG_SPEED_BOOST_GAUGE_CONSUMER_ENABLED;
        g_state.flags |= P5136_XUN_FLAG_REMAINING_PHYSICS_CONSUMERS_ENABLED;
        g_state.flags |= P5136_XUN_FLAG_CHARGER_VISUAL_ENABLED;
        g_state.xun_tacho_registration_id = 0x00684960u;
    }
    if (!exact_build) {
        g_state.status = P5136_XUN_STATUS_UNSUPPORTED_PROCESS;
    } else if (!enabled) {
        g_state.status = P5136_XUN_STATUS_EXACT_BUILD_DISABLED;
    } else if (!hooks_installed) {
        g_state.status = P5136_XUN_STATUS_INITIALIZATION_FAILED;
    } else {
        g_state.status = P5136_XUN_STATUS_XUN_TACHO_ALIAS_READY;
    }
    ReleaseSRWLockExclusive(&g_state_lock);

    if (!exact_build) {
        append_log(L"unsupported process; no hooks applied");
    } else if (!enabled) {
        append_log(L"exact P5136 build verified; sidecar disabled; no hooks applied");
    } else if (!hooks_installed) {
        append_log(hook_error[0] == L'\0' ? L"lifecycle probe installation failed" : hook_error);
    } else {
        append_log(L"exact P5136 build verified; XUN factory, dashboard ABI, physics, and visual hooks installed; consumers await a server-selected kart profile");
    }
    return TRUE;
}

}  // namespace

void p5136_xun_set_module(HMODULE module) noexcept {
    g_module = module;
}

void p5136_xun_set_proxy_ready(bool ready) noexcept {
    AcquireSRWLockExclusive(&g_state_lock);
    if (ready) {
        g_state.flags |= P5136_XUN_FLAG_PROXY_READY;
    } else {
        g_state.flags &= ~P5136_XUN_FLAG_PROXY_READY;
    }
    ReleaseSRWLockExclusive(&g_state_lock);
}

void p5136_xun_initialize() noexcept {
    InitOnceExecuteOnce(&g_initialize_once, initialize_once, nullptr, nullptr);
}

P5136XunStatusSnapshot p5136_xun_snapshot() noexcept {
    p5136_xun_initialize();
    AcquireSRWLockShared(&g_state_lock);
    P5136XunStatusSnapshot result = g_state;
    ReleaseSRWLockShared(&g_state_lock);
    const P5136XunPhysicsProbeSnapshot physics = p5136_xun_physics_probe_snapshot();
    result.drive_event_calls = physics.drive_event_calls;
    result.physics_tick_calls = physics.physics_tick_calls;
    result.charger_activations = physics.charger_activations;
    result.charger_deactivations = physics.charger_deactivations;
    result.active_charger_karts = physics.active_karts;
    result.speed_boost_gauge_adjustments = physics.speed_boost_gauge_adjustments;
    result.charger_resource_registrations = physics.charger_resource_registrations;
    result.charger_effect_spawns = physics.charger_effect_spawns;
    result.charger_effect_removals = physics.charger_effect_removals;
    return result;
}

void p5136_xun_log_message(const wchar_t* message) noexcept {
    append_log(message);
}

void p5136_xun_log_server_profile(
    uint32_t generation,
    uint16_t kart_id,
    uint8_t exceed_type,
    uint8_t profile_state,
    bool enabled,
    uint32_t booster_use_count,
    uint32_t use_time_ms) noexcept {
    wchar_t message[320] = {};
    wsprintfW(
        message,
        L"server profile: generation=%lu kart=%u exceed_type=%u state=%u enabled=%u boosters=%lu duration_ms=%lu",
        generation,
        static_cast<unsigned int>(kart_id),
        static_cast<unsigned int>(exceed_type),
        static_cast<unsigned int>(profile_state),
        enabled ? 1u : 0u,
        booster_use_count,
        use_time_ms);
    append_log(message);
}

void p5136_xun_log_local_kart_binding(
    const void* kart,
    uint32_t generation,
    uint16_t kart_id) noexcept {
    wchar_t message[224] = {};
    wsprintfW(
        message,
        L"server profile: bound local GoPlayKart=%p generation=%lu kart=%u",
        kart,
        generation,
        static_cast<unsigned int>(kart_id));
    append_log(message);
}

void p5136_xun_log_charger_transition(
    const void* kart,
    bool active,
    uint32_t activation_count,
    uint32_t total_booster_uses,
    uint32_t active_until_ms) noexcept {
    wchar_t message[256] = {};
    wsprintfW(
        message,
        L"physics-state probe: kart=%p charger=%s activations=%lu booster_uses=%lu deadline=%lu",
        kart,
        active ? L"active" : L"inactive",
        activation_count,
        total_booster_uses,
        active_until_ms);
    append_log(message);
}

void p5136_xun_log_drive_event_sample(
    const void* kart,
    uint32_t event_payload,
    uint32_t event_kind,
    bool accepted,
    bool counted_as_booster,
    uint32_t client_booster_uses_before,
    uint32_t client_booster_uses_after,
    uint32_t client_time_ms,
    uint32_t pending_booster_uses,
    bool charger_active) noexcept {
    wchar_t message[512] = {};
    wsprintfW(
        message,
        L"drive-event sample: kart=%p kind=%lu payload=%lu accepted=%u counted=%u client_boosters=%lu->%lu client_time=%lu pending=%lu charger=%s",
        kart,
        event_kind,
        event_payload,
        accepted ? 1u : 0u,
        counted_as_booster ? 1u : 0u,
        client_booster_uses_before,
        client_booster_uses_after,
        client_time_ms,
        pending_booster_uses,
        charger_active ? L"active" : L"inactive");
    append_log(message);
}

void p5136_xun_log_physics_tick_sample(
    const void* kart,
    uint32_t client_time_ms,
    uint32_t tick_calls,
    uint32_t total_booster_uses,
    uint32_t pending_booster_uses,
    bool charger_active,
    uint32_t active_until_ms) noexcept {
    wchar_t message[384] = {};
    wsprintfW(
        message,
        L"physics-tick sample: kart=%p client_time=%lu ticks=%lu boosters=%lu pending=%lu charger=%s deadline=%lu",
        kart,
        client_time_ms,
        tick_calls,
        total_booster_uses,
        pending_booster_uses,
        charger_active ? L"active" : L"inactive",
        active_until_ms);
    append_log(message);
}

void p5136_xun_log_speed_boost_gauge_adjustment(
    const void* kart,
    uint32_t adjustment_count,
    float original_addend,
    float scaled_addend,
    float multiplier) noexcept {
    wchar_t message[320] = {};
    swprintf_s(
        message,
        L"speed boost-gauge consumer: kart=%p count=%lu addend=%.9g->%.9g multiplier=%.9g",
        kart,
        adjustment_count,
        static_cast<double>(original_addend),
        static_cast<double>(scaled_addend),
        static_cast<double>(multiplier));
    append_log(message);
}

void p5136_xun_log_charger_visual(
    const void* kart,
    const wchar_t* action,
    const void* effect) noexcept {
    wchar_t message[256] = {};
    wsprintfW(
        message,
        L"charger visual: kart=%p action=%s renderer_object=%p",
        kart,
        action,
        effect);
    append_log(message);
}

void p5136_xun_log_exceed_gauge(
    const void* tacho,
    const void* controller,
    float normalized_fill,
    bool feature_present,
    bool active,
    bool usable,
    uint32_t visual_state) noexcept {
    wchar_t message[320] = {};
    swprintf_s(
        message,
        L"tachometer Exceed gauge: tacho=%p controller=%p fill=%.3f present=%u active=%u usable=%u visual=%lu",
        tacho,
        controller,
        static_cast<double>(normalized_fill),
        feature_present ? 1u : 0u,
        active ? 1u : 0u,
        usable ? 1u : 0u,
        visual_state);
    append_log(message);
}

void record_tacho_allocator(bool v1) noexcept {
    const DWORD thread_id = GetCurrentThreadId();
    uint32_t count = 0;
    AcquireSRWLockExclusive(&g_state_lock);
    if (v1) {
        count = ++g_state.v1_tacho_allocator_calls;
        g_state.v1_tacho_last_thread_id = thread_id;
    } else {
        count = ++g_state.x_tacho_allocator_calls;
        g_state.x_tacho_last_thread_id = thread_id;
    }
    ReleaseSRWLockExclusive(&g_state_lock);

    if (count <= 64 || (count & (count - 1)) == 0) {
        wchar_t message[160] = {};
        wsprintfW(
            message,
            L"lifecycle probe: %s tachometer allocator call=%lu thread=%lu",
            v1 ? L"V1" : L"X",
            count,
            thread_id);
        append_log(message);
    }
}

extern "C" uint32_t P5136_XUN_CALL P5136XunGetAbiVersion() {
    return P5136_XUN_ABI_VERSION;
}

extern "C" uint32_t P5136_XUN_CALL P5136XunInitialize(void* reserved) {
    static_cast<void>(reserved);
    return p5136_xun_snapshot().status;
}

extern "C" int32_t P5136_XUN_CALL P5136XunGetStatus(P5136XunStatusSnapshot* output) {
    if (output == nullptr || output->size < sizeof(P5136XunStatusSnapshot)) {
        return 0;
    }
    *output = p5136_xun_snapshot();
    return 1;
}

extern "C" int32_t P5136_XUN_CALL P5136XunVerifyExecutableFile(const wchar_t* path) {
    return is_exact_build(read_file_identity(path)) ? 1 : 0;
}

extern "C" void __cdecl p5136_xun_record_v1_tacho_allocator() noexcept {
    record_tacho_allocator(true);
}

extern "C" void __cdecl p5136_xun_record_x_tacho_allocator() noexcept {
    record_tacho_allocator(false);
}
