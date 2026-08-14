#include <winsock2.h>
#include <ws2tcpip.h>
#include <windows.h>

#include "xun_profile_client.hpp"

#include <array>
#include <algorithm>
#include <cmath>
#include <cstdint>
#include <cstring>

#include "sidecar.hpp"
#include "xun_physics_runtime.hpp"

namespace {

constexpr std::array<std::uint8_t, 4> kHandshakeMagic = {'P', '5', 'X', 'C'};
constexpr std::array<std::uint8_t, 4> kProfileMagic = {'P', '5', 'X', 'P'};
constexpr std::array<std::uint8_t, 4> kClientEventMagic = {'P', '5', 'X', 'E'};
constexpr std::uint16_t kProtocolVersion = 2;
constexpr std::size_t kProfileFrameLength = 52;
constexpr std::size_t kClientEventFrameLength = 12;
constexpr std::uint16_t kRaceResetEventType = 1;
constexpr std::size_t kMaximumNicknameBytes = 128;
constexpr DWORD kReconnectDelayMs = 1500;

volatile LONG g_profile_client_started = 0;
volatile LONG g_race_reset_requested = 0;

struct SessionConfiguration {
    wchar_t server[64]{};
    wchar_t nickname[64]{};
    std::uint16_t port = 0;
};

bool executable_directory(wchar_t* output, DWORD capacity) noexcept {
    if (capacity == 0) {
        return false;
    }
    const DWORD length = GetModuleFileNameW(nullptr, output, capacity);
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

bool append_path(wchar_t* path, DWORD capacity, const wchar_t* suffix) noexcept {
    const DWORD length = static_cast<DWORD>(lstrlenW(path));
    const DWORD suffix_length = static_cast<DWORD>(lstrlenW(suffix));
    if (length + suffix_length + 1 > capacity) {
        return false;
    }
    std::memcpy(path + length, suffix, (suffix_length + 1) * sizeof(wchar_t));
    return true;
}

bool read_session_configuration(SessionConfiguration* output) noexcept {
    wchar_t path[MAX_PATH] = {};
    if (output == nullptr
        || !executable_directory(path, MAX_PATH)
        || !append_path(path, MAX_PATH, L"p5136-xun-session.ini")) {
        return false;
    }
    if (GetPrivateProfileIntW(L"session", L"protocol", 0, path) != kProtocolVersion) {
        return false;
    }
    const UINT port = GetPrivateProfileIntW(L"session", L"port", 0, path);
    if (port == 0 || port > 0xFFFFu) {
        return false;
    }
    const DWORD server_length = GetPrivateProfileStringW(
        L"session",
        L"server",
        L"",
        output->server,
        static_cast<DWORD>(_countof(output->server)),
        path);
    const DWORD nickname_length = GetPrivateProfileStringW(
        L"session",
        L"nickname",
        L"",
        output->nickname,
        static_cast<DWORD>(_countof(output->nickname)),
        path);
    if (server_length == 0
        || server_length >= _countof(output->server) - 1
        || nickname_length == 0
        || nickname_length >= _countof(output->nickname) - 1) {
        return false;
    }
    output->port = static_cast<std::uint16_t>(port);
    return true;
}

bool send_all(SOCKET socket, const std::uint8_t* data, std::size_t length) noexcept {
    while (length != 0) {
        const int chunk = send(
            socket,
            reinterpret_cast<const char*>(data),
            static_cast<int>(length),
            0);
        if (chunk <= 0) {
            return false;
        }
        data += chunk;
        length -= static_cast<std::size_t>(chunk);
    }
    return true;
}

bool receive_all(SOCKET socket, std::uint8_t* data, std::size_t length) noexcept {
    while (length != 0) {
        const int chunk = recv(
            socket,
            reinterpret_cast<char*>(data),
            static_cast<int>(length),
            0);
        if (chunk <= 0) {
            return false;
        }
        data += chunk;
        length -= static_cast<std::size_t>(chunk);
    }
    return true;
}

bool send_race_reset_event(SOCKET socket) noexcept {
    std::array<std::uint8_t, kClientEventFrameLength> frame{};
    std::copy(kClientEventMagic.begin(), kClientEventMagic.end(), frame.begin());
    frame[4] = static_cast<std::uint8_t>(kProtocolVersion);
    frame[6] = static_cast<std::uint8_t>(kClientEventFrameLength);
    frame[8] = static_cast<std::uint8_t>(kRaceResetEventType);
    return send_all(socket, frame.data(), frame.size());
}

std::uint16_t read_u16(const std::uint8_t* input) noexcept {
    return static_cast<std::uint16_t>(input[0])
        | static_cast<std::uint16_t>(input[1] << 8u);
}

std::uint32_t read_u32(const std::uint8_t* input) noexcept {
    return static_cast<std::uint32_t>(input[0])
        | (static_cast<std::uint32_t>(input[1]) << 8u)
        | (static_cast<std::uint32_t>(input[2]) << 16u)
        | (static_cast<std::uint32_t>(input[3]) << 24u);
}

float read_float(const std::uint8_t* input) noexcept {
    const std::uint32_t bits = read_u32(input);
    float value = 0.0F;
    std::memcpy(&value, &bits, sizeof(value));
    return value;
}

void apply_profile_frame(const std::array<std::uint8_t, kProfileFrameLength>& frame) noexcept {
    if (!std::equal(kProfileMagic.begin(), kProfileMagic.end(), frame.begin())
        || read_u16(frame.data() + 4) != kProtocolVersion
        || read_u16(frame.data() + 6) != static_cast<std::uint16_t>(kProfileFrameLength)) {
        p5136_xun_log_message(L"profile transport: rejected an invalid server frame");
        return;
    }
    const std::uint32_t generation = read_u32(frame.data() + 8);
    const std::uint16_t kart_id = read_u16(frame.data() + 12);
    const std::uint8_t exceed_type = frame[14];
    const std::uint8_t profile_state = frame[15];
    const std::uint32_t flags = read_u32(frame.data() + 16);

    p5136::xun::ChargerTuning tuning{};
    tuning.booster_use_count = read_u32(frame.data() + 20);
    tuning.use_time_ms = read_u32(frame.data() + 24);
    tuning.charge_boost_by_speed_multiplier = read_float(frame.data() + 28);
    tuning.drift_gauge_factor = read_float(frame.data() + 32);
    tuning.wall_gauge_added = read_float(frame.data() + 36);
    tuning.boost_gauge_added = read_float(frame.data() + 40);
    tuning.anti_collide_balance = read_float(frame.data() + 44);

    const bool values_valid = tuning.booster_use_count > 0
        && tuning.booster_use_count <= 16
        && tuning.use_time_ms > 0
        && tuning.use_time_ms <= 30'000
        && std::isfinite(tuning.charge_boost_by_speed_multiplier)
        && tuning.charge_boost_by_speed_multiplier >= 1.0F
        && tuning.charge_boost_by_speed_multiplier <= 1000.0F
        && std::isfinite(tuning.drift_gauge_factor)
        && tuning.drift_gauge_factor >= 0.0F
        && tuning.drift_gauge_factor <= 10.0F
        && std::isfinite(tuning.wall_gauge_added)
        && tuning.wall_gauge_added >= 0.0F
        && tuning.wall_gauge_added <= 1.0F
        && std::isfinite(tuning.boost_gauge_added)
        && tuning.boost_gauge_added >= 0.0F
        && tuning.boost_gauge_added <= 1.0F
        && std::isfinite(tuning.anti_collide_balance)
        && tuning.anti_collide_balance >= 0.0F
        && tuning.anti_collide_balance <= 10.0F;
    tuning.enabled = profile_state == 1u && values_valid;
    tuning.apply_speed_boost_gauge = tuning.enabled && (flags & 1u) != 0;
    tuning.apply_remaining_consumers = tuning.enabled && (flags & 2u) != 0;
    if (!tuning.enabled) {
        tuning = {};
    }
    p5136_xun_apply_server_profile(
        tuning,
        generation,
        kart_id,
        exceed_type,
        profile_state,
        frame[48],
        frame[49],
        frame[50],
        frame[51]);
}

bool connect_and_run(const SessionConfiguration& configuration) noexcept {
    sockaddr_in endpoint{};
    endpoint.sin_family = AF_INET;
    endpoint.sin_port = htons(configuration.port);
    if (InetPtonW(AF_INET, configuration.server, &endpoint.sin_addr) != 1) {
        return false;
    }
    const SOCKET socket = ::socket(AF_INET, SOCK_STREAM, IPPROTO_TCP);
    if (socket == INVALID_SOCKET) {
        return false;
    }
    bool success = connect(
        socket,
        reinterpret_cast<const sockaddr*>(&endpoint),
        sizeof(endpoint)) == 0;

    std::array<char, 256> nickname{};
    const int nickname_bytes = WideCharToMultiByte(
        CP_UTF8,
        WC_ERR_INVALID_CHARS,
        configuration.nickname,
        -1,
        nickname.data(),
        static_cast<int>(nickname.size()),
        nullptr,
        nullptr);
    if (nickname_bytes <= 1
        || static_cast<std::size_t>(nickname_bytes - 1) > kMaximumNicknameBytes) {
        success = false;
    }
    std::array<std::uint8_t, 8 + kMaximumNicknameBytes> handshake{};
    if (success) {
        std::copy(kHandshakeMagic.begin(), kHandshakeMagic.end(), handshake.begin());
        handshake[4] = static_cast<std::uint8_t>(kProtocolVersion);
        handshake[5] = 0;
        const std::uint16_t nickname_length = static_cast<std::uint16_t>(nickname_bytes - 1);
        handshake[6] = static_cast<std::uint8_t>(nickname_length & 0xFFu);
        handshake[7] = static_cast<std::uint8_t>(nickname_length >> 8u);
        std::memcpy(handshake.data() + 8, nickname.data(), nickname_length);
        success = send_all(socket, handshake.data(), 8u + nickname_length);
    }
    if (success) {
        p5136_xun_log_message(L"profile transport: connected to the server sidecar endpoint");
    }
    while (success) {
        if (InterlockedExchange(&g_race_reset_requested, 0) != 0) {
            success = send_race_reset_event(socket);
            if (success) {
                p5136_xun_log_message(
                    L"profile transport: sent a race-reset generation request");
            }
        }
        if (!success) {
            break;
        }
        fd_set readable{};
        FD_SET(socket, &readable);
        timeval poll_interval{};
        poll_interval.tv_usec = 100'000;
        const int ready = select(0, &readable, nullptr, nullptr, &poll_interval);
        if (ready == SOCKET_ERROR) {
            success = false;
            break;
        }
        if (ready == 0) {
            continue;
        }
        std::array<std::uint8_t, kProfileFrameLength> frame{};
        success = receive_all(socket, frame.data(), frame.size());
        if (success) {
            apply_profile_frame(frame);
        }
    }
    p5136_xun_log_message(L"profile transport: disconnected before the next profile frame");
    closesocket(socket);
    return false;
}

DWORD WINAPI profile_client_thread(void*) noexcept {
    WSADATA winsock{};
    if (WSAStartup(MAKEWORD(2, 2), &winsock) != 0) {
        p5136_xun_log_message(L"profile transport: WSAStartup failed");
        return 0;
    }
    for (;;) {
        SessionConfiguration configuration{};
        if (read_session_configuration(&configuration)) {
            static_cast<void>(connect_and_run(configuration));
        }
        p5136_xun_reset_server_profile();
        Sleep(kReconnectDelayMs);
    }
}

}  // namespace

void p5136_xun_request_race_reset() noexcept {
    InterlockedExchange(&g_race_reset_requested, 1);
}

void p5136_xun_start_profile_client() noexcept {
    if (InterlockedCompareExchange(&g_profile_client_started, 1, 0) != 0) {
        return;
    }
    HANDLE thread = CreateThread(nullptr, 0, profile_client_thread, nullptr, 0, nullptr);
    if (thread == nullptr) {
        InterlockedExchange(&g_profile_client_started, 0);
        p5136_xun_log_message(L"profile transport: failed to create the network thread");
        return;
    }
    SetThreadPriority(thread, THREAD_PRIORITY_BELOW_NORMAL);
    CloseHandle(thread);
}
