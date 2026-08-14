#include "lifecycle_hooks.hpp"

#include <windows.h>
#include <tlhelp32.h>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <limits>
#include <vector>

#include "charger_visual.hpp"
#include "sidecar.hpp"
#include "xun_physics_runtime.hpp"

extern "C" void* g_p5136_xun_v1_tacho_trampoline = nullptr;
extern "C" void* g_p5136_xun_x_tacho_trampoline = nullptr;
extern "C" void* g_p5136_xun_tacho_factory_lookup_trampoline = nullptr;
extern "C" void* g_p5136_xun_v1_tacho_update_trampoline = nullptr;
extern "C" void* g_p5136_xun_display_stat_trampoline = nullptr;
extern "C" void* g_p5136_xun_drive_event_trampoline = nullptr;
extern "C" void* g_p5136_xun_physics_tick_trampoline = nullptr;
extern "C" void* g_p5136_add_protected_float = nullptr;
extern "C" void* g_p5136_get_protected_float = nullptr;
extern "C" void* g_p5136_set_collision_response = nullptr;
extern "C" void* g_p5136_string_c_str = nullptr;
extern "C" void* g_p5136_copy_shared_ref = nullptr;
extern "C" void* g_p5136_v1_flat_gauge_allocator = nullptr;
extern "C" void* g_p5136_flat_gauge_config_ctor = nullptr;
extern "C" void* g_p5136_wide_string_assign = nullptr;
extern "C" void* g_p5136_flat_gauge_bind = nullptr;
extern "C" void* g_p5136_flat_gauge_config_dtor = nullptr;
extern "C" void* g_p5136_shared_ref_get = nullptr;
extern "C" void* g_p5136_rect_copy = nullptr;
extern "C" void* g_p5136_rect_width = nullptr;
extern "C" void* g_p5136_set_panel_uv_rect = nullptr;
extern "C" void* g_p5136_display_protected_float = nullptr;

extern "C" void* g_p5136_xun_tacho_object = nullptr;

namespace {

volatile LONG g_last_exceed_gauge_signature = -1;
volatile LONG g_last_charger_dashboard_signature = -1;
volatile LONG g_last_crash_gauge_signature = -1;
volatile LONG g_last_crash_gauge_rearm_ms = 0;
void* g_charger_gauge_tacho = nullptr;
void* g_charger_gauge_controller = nullptr;
float g_charger_gauge_displayed_fraction = 0.0F;
std::array<void*, 14> g_charger_rect_gauge_vtable{};

using GetProtectedFloat = float(__thiscall*)(void*);
using SetCollisionResponse = float(__thiscall*)(void*, float);
using CopySharedRef = void*(__thiscall*)(void*, const void*);
using AllocateFlatGauge = void*(__cdecl*)();
using ConstructGaugeConfig = void*(__thiscall*)(void*);
using AssignWideString = void*(__thiscall*)(void*, const wchar_t*);
using BindFlatGauge = std::int32_t(__thiscall*)(void*, void*, void*);
using DestroyGaugeConfig = void(__thiscall*)(void*);
using SharedRefGet = void*(__thiscall*)(void*);
using CopyGaugeRect = void*(__thiscall*)(void*, const void*);
using RectWidth = float(__thiscall*)(const void*);
using SetPanelUvRect = void(__thiscall*)(void*, const void*);
// The tachometer factory is keyed by the client's UTF-16 string type. The
// same accessor-shaped routine exists for narrow strings elsewhere, so the
// live factory frame is the authoritative type evidence here.
using FactoryStringCStr = const wchar_t*(__thiscall*)(const void*);
using V1TachoUpdate = std::int32_t(__thiscall*)(
    void*,
    std::uint32_t,
    void*);
using DisplayStatConverter = std::int32_t(__cdecl*)(
    std::uint32_t,
    std::int32_t,
    void*,
    std::int16_t*);
}  // namespace

extern "C" std::int32_t __stdcall p5136_xun_is_tacho_factory_request(
    const void* name) noexcept {
    if (name == nullptr || g_p5136_string_c_str == nullptr) {
        return 0;
    }
    const wchar_t* text = reinterpret_cast<FactoryStringCStr>(g_p5136_string_c_str)(name);
    if (text == nullptr || lstrcmpiW(text, L"XunGenTacho") != 0) {
        return 0;
    }
    static volatile LONG logged = 0;
    if (InterlockedCompareExchange(&logged, 1, 0) == 0) {
        p5136_xun_log_message(
            L"tachometer factory: XunGenTacho resolved through the P5136 V1 ABI");
    }
    return 1;
}

extern "C" void* __stdcall p5136_xun_record_tacho_object(void* object) noexcept {
    InterlockedExchangePointer(&g_p5136_xun_tacho_object, object);
    InterlockedExchange(&g_last_exceed_gauge_signature, -1);
    InterlockedExchange(&g_last_charger_dashboard_signature, -1);
    InterlockedExchange(&g_last_crash_gauge_signature, -1);
    InterlockedExchange(&g_last_crash_gauge_rearm_ms, 0);
    g_charger_gauge_tacho = nullptr;
    g_charger_gauge_controller = nullptr;
    g_charger_gauge_displayed_fraction = 0.0F;
    return object;
}

// P5136 already implements the V1/XUN crash gauge in GoPlayKart:
//
//   A40150 -> A3FFD0 -> A401E0
//
// It is deliberately distinct from chargeInstAccelGaugeByWall, which feeds
// ordinary Exceed. Keep this probe read-only so a healthy native transfer is
// never awarded twice. The asynchronous sidecar logger gives live evidence
// for the collision sample, its pending normalized transfer, and the ordinary
// booster gauge that receives it.
void log_xun_crash_gauge_state(void* tacho, void* kart) noexcept {
    if (tacho == nullptr
        || tacho != g_p5136_xun_tacho_object
        || kart == nullptr
        || g_p5136_get_protected_float == nullptr) {
        return;
    }

    float gauge_cap = 0.0F;
    float booster_gauge = 0.0F;
    float pending_ratio = 0.0F;
    float pending_transfer = 0.0F;
    std::uint32_t sample_started_ms = 0;
    std::uint32_t rearm_ms = 0;
    __try {
        auto* bytes = static_cast<std::uint8_t*>(kart);
        const auto get_float = reinterpret_cast<GetProtectedFloat>(
            g_p5136_get_protected_float);
        gauge_cap = get_float(bytes + 0x0734u);
        booster_gauge = get_float(bytes + 0x0A90u);
        pending_transfer = get_float(bytes + 0x0EF4u);
        pending_ratio = get_float(bytes + 0x0F00u);
        sample_started_ms = *reinterpret_cast<const std::uint32_t*>(bytes + 0x0F10u);
        rearm_ms = *reinterpret_cast<const std::uint32_t*>(bytes + 0x0F14u);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return;
    }
    if (!std::isfinite(gauge_cap)
        || !std::isfinite(booster_gauge)
        || !std::isfinite(pending_ratio)
        || !std::isfinite(pending_transfer)) {
        return;
    }

    // Log only collision-path state transitions. Ordinary drift gauge changes
    // alone must not turn this into a per-frame logger.
    const LONG active = sample_started_ms != 0 ? 1L : 0L;
    const LONG pending_bucket = static_cast<LONG>(
        std::clamp(pending_ratio, 0.0F, 1.0F) * 100.0F + 0.5F);
    const LONG signature = active | (pending_bucket << 1);
    const LONG previous = InterlockedExchange(
        &g_last_crash_gauge_signature,
        signature);
    const LONG previous_rearm = InterlockedExchange(
        &g_last_crash_gauge_rearm_ms,
        static_cast<LONG>(rearm_ms));
    const bool rearmed = rearm_ms != 0
        && static_cast<std::uint32_t>(previous_rearm) != rearm_ms;
    if (previous == signature && !rearmed) {
        return;
    }
    wchar_t message[512] = {};
    wsprintfW(
        message,
        L"native crash gauge: kart=%p sample_start=%lu rearm=%lu pending_ratio=%ld%% pending_transfer_x100=%ld ordinary_boost_x100=%ld cap_x100=%ld",
        kart,
        sample_started_ms,
        rearm_ms,
        pending_bucket,
        static_cast<LONG>(pending_transfer * 100.0F),
        static_cast<LONG>(booster_gauge * 100.0F),
        static_cast<LONG>(gauge_cap * 100.0F));
    p5136_xun_log_message(message);
}

void log_xun_exceed_gauge_state(void* tacho) noexcept {
    if (tacho == nullptr || tacho != g_p5136_xun_tacho_object) {
        return;
    }

    void* controller = nullptr;
    float fill = 0.0F;
    bool present = false;
    bool active = false;
    bool usable = false;
    std::uint32_t visual_state = 0;
    __try {
        const auto bytes = static_cast<const std::uint8_t*>(tacho);
        controller = *reinterpret_cast<void* const*>(bytes + 0x01E4u);
        present = bytes[0x0288u] != 0;
        active = bytes[0x0289u] != 0;
        usable = bytes[0x028Au] != 0;
        visual_state = *reinterpret_cast<const std::uint32_t*>(bytes + 0x028Cu);
        if (controller != nullptr) {
            fill = *reinterpret_cast<const float*>(
                static_cast<const std::uint8_t*>(controller) + 0x0Cu);
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return;
    }
    if (!std::isfinite(fill)) {
        return;
    }

    const float clamped = fill < 0.0F ? 0.0F : (fill > 1.0F ? 1.0F : fill);
    const LONG bucket = static_cast<LONG>(clamped * 20.0F + 0.5F);
    const LONG signature = bucket
        | (present ? 1L << 8 : 0)
        | (active ? 1L << 9 : 0)
        | (usable ? 1L << 10 : 0)
        | (static_cast<LONG>(visual_state & 3u) << 11);
    const LONG previous = InterlockedExchange(
        &g_last_exceed_gauge_signature,
        signature);
    if (previous != signature) {
        p5136_xun_log_exceed_gauge(
            tacho,
            controller,
            clamped,
            present,
            active,
            usable,
            visual_state);
    }
}

extern "C" std::int32_t __fastcall p5136_xun_rect_gauge_set_fraction(
    void* controller,
    void*,
    float requested_fraction) noexcept {
    if (controller == nullptr) {
        return 0;
    }
    const float fraction = requested_fraction < 0.0F
        ? 0.0F
        : (requested_fraction > 1.0F ? 1.0F : requested_fraction);
    const auto copy_rect = reinterpret_cast<CopyGaugeRect>(g_p5136_rect_copy);
    const auto rect_width = reinterpret_cast<RectWidth>(g_p5136_rect_width);
    const auto shared_get = reinterpret_cast<SharedRefGet>(g_p5136_shared_ref_get);
    const auto set_uv = reinterpret_cast<SetPanelUvRect>(g_p5136_set_panel_uv_rect);
    if (copy_rect == nullptr || rect_width == nullptr || shared_get == nullptr || set_uv == nullptr) {
        return 0;
    }

    __try {
        auto* bytes = static_cast<std::uint8_t*>(controller);
        std::array<float, 4> destination{};
        copy_rect(destination.data(), bytes + 0x28u);
        destination[1] = *reinterpret_cast<const float*>(bytes + 0x34u)
            - rect_width(bytes + 0x28u) * fraction;

        std::array<float, 4> uv{};
        copy_rect(uv.data(), bytes + 0x38u);
        uv[1] = *reinterpret_cast<const float*>(bytes + 0x44u)
            - rect_width(bytes + 0x38u) * fraction;

        for (const std::size_t offset : {0x18u, 0x1Cu, 0x20u}) {
            void* panel = shared_get(bytes + offset);
            if (panel == nullptr) {
                continue;
            }
            auto** vtable = *reinterpret_cast<void***>(panel);
            using SetWindowRect = void(__thiscall*)(void*, const void*);
            reinterpret_cast<SetWindowRect>(vtable[9])(panel, destination.data());
            set_uv(panel, uv.data());
        }
        *reinterpret_cast<float*>(bytes + 0x0Cu) = fraction;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return 0;
    }
    return 1;
}

bool ensure_xun_charger_gauge(void* tacho) noexcept {
    if (tacho == nullptr) {
        return false;
    }
    if (g_charger_gauge_tacho == tacho && g_charger_gauge_controller != nullptr) {
        return true;
    }
    const auto allocate = reinterpret_cast<AllocateFlatGauge>(
        g_p5136_v1_flat_gauge_allocator);
    const auto construct_config = reinterpret_cast<ConstructGaugeConfig>(
        g_p5136_flat_gauge_config_ctor);
    const auto assign = reinterpret_cast<AssignWideString>(g_p5136_wide_string_assign);
    const auto copy_shared = reinterpret_cast<CopySharedRef>(g_p5136_copy_shared_ref);
    const auto bind = reinterpret_cast<BindFlatGauge>(g_p5136_flat_gauge_bind);
    const auto destroy_config = reinterpret_cast<DestroyGaugeConfig>(
        g_p5136_flat_gauge_config_dtor);
    if (allocate == nullptr
        || construct_config == nullptr
        || assign == nullptr
        || copy_shared == nullptr
        || bind == nullptr
        || destroy_config == nullptr) {
        return false;
    }

    void* controller = nullptr;
    alignas(void*) std::array<std::uint8_t, 24> config{};
    bool config_constructed = false;
    __try {
        controller = allocate();
        if (controller == nullptr) {
            return false;
        }
        construct_config(config.data());
        config_constructed = true;
        assign(config.data() + 0x00u, L"instCharger");
        assign(config.data() + 0x0Cu, L"instChargerGauge");
        assign(config.data() + 0x10u, L"instChargerGaugeOn");
        assign(config.data() + 0x08u, L"instChargerFullFrame");

        void* root_reference = nullptr;
        copy_shared(&root_reference, static_cast<std::uint8_t*>(tacho) + 0x01ECu);
        bind(controller, root_reference, config.data());
        destroy_config(config.data());
        config_constructed = false;

        auto** original_vtable = *reinterpret_cast<void***>(controller);
        for (std::size_t index = 0; index < g_charger_rect_gauge_vtable.size(); ++index) {
            g_charger_rect_gauge_vtable[index] = original_vtable[index];
        }
        g_charger_rect_gauge_vtable[12] = reinterpret_cast<void*>(
            &p5136_xun_rect_gauge_set_fraction);
        *reinterpret_cast<void***>(controller) = g_charger_rect_gauge_vtable.data();
        g_charger_gauge_tacho = tacho;
        g_charger_gauge_controller = controller;
        g_charger_gauge_displayed_fraction = 0.0F;
        p5136_xun_log_message(
            L"tachometer charger gauge: bound native continuous flat-gauge controller");
        return true;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        if (config_constructed) {
            __try {
                destroy_config(config.data());
            } __except (EXCEPTION_EXECUTE_HANDLER) {
            }
        }
        p5136_xun_log_message(
            L"tachometer charger gauge: native controller binding failed");
        return false;
    }
}

void update_xun_charger_gauge(
    void* tacho,
    void* kart,
    std::uint32_t client_time_ms) noexcept {
    if (tacho == nullptr || tacho != g_p5136_xun_tacho_object) {
        return;
    }
    P5136XunDashboardState state{};
    const bool enabled = p5136_xun_dashboard_state(kart, &state);
    if (!ensure_xun_charger_gauge(tacho)) {
        return;
    }

    float target = 0.0F;
    if (enabled && state.required_booster_uses != 0) {
        if (state.active && state.active_duration_ms != 0) {
            const std::uint32_t remaining = client_time_ms < state.active_until_ms
                ? state.active_until_ms - client_time_ms
                : 0;
            target = static_cast<float>(remaining)
                / static_cast<float>(state.active_duration_ms);
        } else {
            target = static_cast<float>(state.pending_booster_uses)
                / static_cast<float>(state.required_booster_uses);
        }
    }
    target = target < 0.0F ? 0.0F : (target > 1.0F ? 1.0F : target);
    if (!state.active && target > g_charger_gauge_displayed_fraction) {
        const float step = state.required_booster_uses == 0
            ? 1.0F
            : 0.1F / static_cast<float>(state.required_booster_uses);
        g_charger_gauge_displayed_fraction = std::min(
            target,
            g_charger_gauge_displayed_fraction + step);
    } else {
        g_charger_gauge_displayed_fraction = target;
    }
    auto** vtable = *reinterpret_cast<void***>(g_charger_gauge_controller);
    using SetGaugeFraction = std::int32_t(__thiscall*)(void*, float);
    reinterpret_cast<SetGaugeFraction>(vtable[12])(
        g_charger_gauge_controller,
        g_charger_gauge_displayed_fraction);

    const LONG bucket = static_cast<LONG>(g_charger_gauge_displayed_fraction * 100.0F + 0.5F);
    const LONG signature = bucket
        | (static_cast<LONG>(state.required_booster_uses & 0xFFu) << 8)
        | (enabled ? 1L << 16 : 0)
        | (state.active ? 1L << 17 : 0);
    if (InterlockedExchange(&g_last_charger_dashboard_signature, signature) != signature) {
        wchar_t message[384] = {};
        wsprintfW(
            message,
            L"tachometer charger gauge: tacho=%p update_arg=%p enabled=%u progress=%lu/%lu active=%u fill=%ld%% controller=%p",
            tacho,
            kart,
            enabled ? 1u : 0u,
            state.pending_booster_uses,
            state.required_booster_uses,
            state.active ? 1u : 0u,
            bucket,
            g_charger_gauge_controller);
        p5136_xun_log_message(message);
    }
}

extern "C" __declspec(naked) void p5136_xun_v1_tacho_probe() {
    __asm {
        pushfd
        pushad
        call p5136_xun_record_v1_tacho_allocator
        popad
        popfd
        jmp dword ptr [g_p5136_xun_v1_tacho_trampoline]
    }
}

extern "C" __declspec(naked) void p5136_xun_x_tacho_probe() {
    __asm {
        pushfd
        pushad
        call p5136_xun_record_x_tacho_allocator
        popad
        popfd
        jmp dword ptr [g_p5136_xun_x_tacho_trampoline]
    }
}

extern "C" __declspec(naked) void p5136_xun_tacho_factory_lookup_probe() {
    __asm {
        push ecx
        push dword ptr [esp + 8]
        call p5136_xun_is_tacho_factory_request
        pop ecx
        test eax, eax
        jz pass_through
        call dword ptr [g_p5136_xun_v1_tacho_trampoline]
        push eax
        call p5136_xun_record_tacho_object
        ret 4
pass_through:
        jmp dword ptr [g_p5136_xun_tacho_factory_lookup_trampoline]
    }
}

extern "C" std::int32_t __fastcall p5136_xun_v1_tacho_update_probe(
    void* tacho,
    void*,
    std::uint32_t client_time_ms,
    void* kart) noexcept {
    const std::int32_t result = reinterpret_cast<V1TachoUpdate>(
        g_p5136_xun_v1_tacho_update_trampoline)(
        tacho,
        client_time_ms,
        kart);
    log_xun_exceed_gauge_state(tacho);
    update_xun_charger_gauge(tacho, kart, client_time_ms);
    log_xun_crash_gauge_state(tacho, kart);
    return result;
}

extern "C" std::int32_t __cdecl p5136_xun_display_stat_probe(
    std::uint32_t category,
    std::int32_t legacy_argument,
    void* spec,
    std::int16_t* output) noexcept {
    const std::int32_t original = reinterpret_cast<DisplayStatConverter>(
        g_p5136_xun_display_stat_trampoline)(
        category,
        legacy_argument,
        spec,
        output);
    if (spec == nullptr || output == nullptr || category >= 4) {
        return original;
    }
    constexpr std::array<std::size_t, 4> kBodyValueOffsets = {
        0x009Cu,
        0x0074u,
        0x004Cu,
        0x000Cu,
    };
    float body_value = 0.0F;
    std::int16_t corrected = 0;
    __try {
        body_value = reinterpret_cast<GetProtectedFloat>(
            g_p5136_display_protected_float)(
            static_cast<std::uint8_t*>(spec) + kBodyValueOffsets[category]);
        if (!p5136_xun_correct_display_stat(category, body_value, &corrected)) {
            return original;
        }
        *output = corrected;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return original;
    }

    static volatile LONG logged_categories = 0;
    const LONG bit = 1L << category;
    if ((InterlockedOr(&logged_categories, bit) & bit) == 0) {
        wchar_t message[256] = {};
        wsprintfW(
            message,
            L"XUN display conversion: category=%lu body_milli=%ld corrected=%d",
            category,
            static_cast<LONG>(body_value * 1000.0F),
            static_cast<int>(corrected));
        p5136_xun_log_message(message);
    }
    return static_cast<std::int32_t>(corrected);
}

extern "C" __declspec(naked) void p5136_xun_drive_event_probe() {
    __asm {
        pushfd
        pushad
        sub esp, 528
        lea ebp, dword ptr [esp + 15]
        and ebp, 0FFFFFFF0h
        fxsave [ebp]
        mov esi, dword ptr [esp + 536]
        mov eax, dword ptr [esi - 8]
        mov edx, dword ptr [esi + 8]
        movzx ecx, byte ptr [esi + 12]
        mov ebx, dword ptr [eax + 0DD4h]
        mov edi, ebx
        cmp ecx, 3
        je booster_event
        cmp ecx, 4
        jne counter_ready
booster_event:
        cmp byte ptr [eax + 05D0h], 0
        jne counter_ready
        sub edi, 1
counter_ready:
        push ebx
        push edi
        push 1
        push ecx
        push edx
        push eax
        call p5136_xun_observe_drive_event
        add esp, 24
        fxrstor [ebp]
        add esp, 528
        popad
        popfd
        jmp dword ptr [g_p5136_xun_drive_event_trampoline]
    }
}

extern "C" __declspec(naked) void p5136_xun_physics_tick_probe() {
    __asm {
        pushfd
        pushad
        mov eax, dword ptr [esp + 24]
        mov edx, dword ptr [esp + 40]
        push edx
        push eax
        call p5136_xun_observe_physics_tick
        add esp, 8
        popad
        popfd
        jmp dword ptr [g_p5136_xun_physics_tick_trampoline]
    }
}

extern "C" __declspec(naked) void p5136_xun_speed_boost_gauge_add_probe() {
    __asm {
        pushfd
        pushad
        sub esp, 528
        lea ebp, dword ptr [esp + 15]
        and ebp, 0FFFFFFF0h
        fxsave [ebp]
        mov eax, dword ptr [esp + 568]
        mov ecx, dword ptr [esp + 552]
        push eax
        push ecx
        call p5136_xun_adjust_speed_boost_gauge_addend
        add esp, 8
        mov dword ptr [esp + 568], eax
        fxrstor [ebp]
        add esp, 528
        popad
        popfd
        jmp dword ptr [g_p5136_add_protected_float]
    }
}

extern "C" float __fastcall p5136_xun_drift_gauge_probe(
    void* protected_value,
    void*) noexcept {
    const float original = reinterpret_cast<GetProtectedFloat>(
        g_p5136_get_protected_float)(protected_value);
    auto* kart = static_cast<std::uint8_t*>(protected_value) - 0x0688u;
    return p5136_xun_drift_gauge_coefficient(kart, original);
}

extern "C" float __fastcall p5136_xun_wall_gauge_probe(
    void* protected_value,
    void*) noexcept {
    const float original = reinterpret_cast<GetProtectedFloat>(
        g_p5136_get_protected_float)(protected_value);
    auto* kart = static_cast<std::uint8_t*>(protected_value) - 0x08B0u;
    return p5136_xun_wall_gauge_coefficient(kart, original);
}

extern "C" float __fastcall p5136_xun_boost_gauge_probe(
    void* protected_value,
    void*) noexcept {
    const float original = reinterpret_cast<GetProtectedFloat>(
        g_p5136_get_protected_float)(protected_value);
    // The call receives either GoPlayKart+0x898 or +0x8A4. The later client
    // adds its XUN value only to the former; a +0x8A4 pointer deliberately
    // derives a non-matching side-table key and therefore remains unchanged.
    auto* kart = static_cast<std::uint8_t*>(protected_value) - 0x0898u;
    return p5136_xun_boost_gauge_coefficient(kart, original);
}

extern "C" float __fastcall p5136_xun_anti_collide_probe(
    void* protected_value,
    void*) noexcept {
    const float original = reinterpret_cast<GetProtectedFloat>(
        g_p5136_get_protected_float)(protected_value);
    auto* kart = static_cast<std::uint8_t*>(protected_value) - 0x084Cu;
    return p5136_xun_anti_collide_balance(kart, original);
}

extern "C" float __fastcall p5136_xun_collision_response_probe(
    void* protected_value,
    void*,
    float original) noexcept {
    auto* kart = static_cast<std::uint8_t*>(protected_value) - 0x0AACu;
    const float adjusted = p5136_xun_collision_response_value(kart, original);
    return reinterpret_cast<SetCollisionResponse>(g_p5136_set_collision_response)(
        protected_value,
        adjusted);
}

namespace {

constexpr std::uintptr_t kV1TachoAllocatorRva = 0x002C2980u;
constexpr std::uintptr_t kXTachoAllocatorRva = 0x002CECF0u;
constexpr std::uintptr_t kTachoFactoryLookupRva = 0x00284960u;
constexpr std::uintptr_t kV1TachoUpdateRva = 0x002C11E0u;
constexpr std::uintptr_t kDriveEventRva = 0x0062D84Du;
constexpr std::uintptr_t kPhysicsTickRva = 0x00633420u;
constexpr std::uintptr_t kAddProtectedFloatRva = 0x00562F00u;
constexpr std::uintptr_t kGetProtectedFloatRva = 0x00269050u;
constexpr std::uintptr_t kSetCollisionResponseRva = 0x006419D0u;
constexpr std::uintptr_t kCopySharedRefRva = 0x000DDD40u;
constexpr std::uintptr_t kV1FlatGaugeAllocatorRva = 0x002C2900u;
constexpr std::uintptr_t kFlatGaugeConfigCtorRva = 0x002C2C80u;
constexpr std::uintptr_t kWideStringAssignRva = 0x00156500u;
constexpr std::uintptr_t kFlatGaugeBindRva = 0x002BEDF0u;
constexpr std::uintptr_t kFlatGaugeConfigDtorRva = 0x002C2EC0u;
constexpr std::uintptr_t kSharedRefGetRva = 0x000DE580u;
constexpr std::uintptr_t kRectCopyRva = 0x0011B640u;
constexpr std::uintptr_t kRectWidthRva = 0x001CEED0u;
constexpr std::uintptr_t kSetPanelUvRectRva = 0x00B3E030u;
constexpr std::uintptr_t kDisplayStatRva = 0x002F64A0u;
constexpr std::uintptr_t kDisplayProtectedFloatRva = 0x00199AF0u;
constexpr std::array<std::uintptr_t, 3> kSpeedBoostGaugeAddCallRvas = {
    0x0063481Du,
    0x0063486Cu,
    0x006348ABu,
};
constexpr std::uintptr_t kDriftGaugeCallRva = 0x006349B0u;
constexpr std::uintptr_t kCollisionResponseCallRva = 0x0063B058u;
constexpr std::array<std::uintptr_t, 2> kAntiCollideCallRvas = {
    0x0063DBC1u,
    0x0063DC36u,
};
constexpr std::uintptr_t kWallGaugeCallRva = 0x0063F8B9u;
constexpr std::uintptr_t kBoostGaugeCallRva = 0x0063FC13u;
constexpr std::size_t kMaximumPatchSize = 9;
constexpr std::size_t kHookCount = 16;
constexpr std::array<BYTE, 6> kExpectedTachoPrologue = {
    0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x08,
};
constexpr std::array<BYTE, 6> kExpectedFactoryLookupPrologue = {
    0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x0C,
};
constexpr std::array<BYTE, 6> kExpectedV1TachoUpdatePrologue = {
    0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x74,
};
constexpr std::array<BYTE, 6> kExpectedDisplayStatPrologue = {
    0x55, 0x8B, 0xEC, 0x83, 0xEC, 0x18,
};
constexpr std::array<BYTE, 7> kExpectedAcceptedDriveEventSite = {
    0x8B, 0x4D, 0x08, 0x51, 0x8B, 0x4D, 0xF8,
};
constexpr std::array<BYTE, 9> kExpectedPhysicsTickPrologue = {
    0x55, 0x8B, 0xEC, 0x81, 0xEC, 0xE0, 0x03, 0x00, 0x00,
};
constexpr std::array<std::array<BYTE, 5>, 3> kExpectedSpeedBoostGaugeAddCalls = {{
    {0xE8, 0xDE, 0xE6, 0xF2, 0xFF},
    {0xE8, 0x8F, 0xE6, 0xF2, 0xFF},
    {0xE8, 0x50, 0xE6, 0xF2, 0xFF},
}};
constexpr std::array<BYTE, 5> kExpectedDriftGaugeCall = {
    0xE8, 0x9B, 0x46, 0xC3, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedCollisionResponseCall = {
    0xE8, 0x73, 0x69, 0x00, 0x00,
};
constexpr std::array<std::array<BYTE, 5>, 2> kExpectedAntiCollideCalls = {{
    {0xE8, 0x8A, 0xB4, 0xC2, 0xFF},
    {0xE8, 0x15, 0xB4, 0xC2, 0xFF},
}};
constexpr std::array<BYTE, 5> kExpectedWallGaugeCall = {
    0xE8, 0x92, 0x97, 0xC2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedBoostGaugeCall = {
    0xE8, 0x38, 0x94, 0xC2, 0xFF,
};
struct HookSite {
    BYTE* target;
    const void* detour;
    void** trampoline_slot;
    const BYTE* expected;
    std::size_t patch_size;
    BYTE opcode = 0xE9;
};

struct SuspendedThread {
    HANDLE handle;
    DWORD thread_id;
};

void set_error(wchar_t* output, std::size_t capacity, const wchar_t* message) noexcept {
    if (output == nullptr || capacity == 0) {
        return;
    }
    const std::size_t bounded = capacity > static_cast<std::size_t>(std::numeric_limits<int>::max())
        ? static_cast<std::size_t>(std::numeric_limits<int>::max())
        : capacity;
    lstrcpynW(output, message, static_cast<int>(bounded));
}

bool encode_relative(
    BYTE* output,
    const void* instruction,
    const void* destination,
    BYTE opcode) noexcept {
    const auto source = reinterpret_cast<std::intptr_t>(instruction);
    const auto target = reinterpret_cast<std::intptr_t>(destination);
    const std::int64_t displacement = static_cast<std::int64_t>(target)
        - static_cast<std::int64_t>(source + 5);
    if (displacement < std::numeric_limits<std::int32_t>::min()
        || displacement > std::numeric_limits<std::int32_t>::max()) {
        return false;
    }
    output[0] = opcode;
    const std::int32_t relative = static_cast<std::int32_t>(displacement);
    std::memcpy(output + 1, &relative, sizeof(relative));
    return true;
}

void* create_trampoline(const HookSite& site) noexcept {
    const std::size_t trampoline_size = site.patch_size + 5;
    auto* trampoline = static_cast<BYTE*>(VirtualAlloc(
        nullptr,
        trampoline_size,
        MEM_COMMIT | MEM_RESERVE,
        PAGE_READWRITE));
    if (trampoline == nullptr) {
        return nullptr;
    }
    std::memcpy(trampoline, site.target, site.patch_size);
    if (!encode_relative(
            trampoline + site.patch_size,
            trampoline + site.patch_size,
            site.target + site.patch_size,
            0xE9)) {
        VirtualFree(trampoline, 0, MEM_RELEASE);
        return nullptr;
    }
    DWORD previous = 0;
    if (VirtualProtect(trampoline, trampoline_size, PAGE_EXECUTE_READ, &previous) == FALSE) {
        VirtualFree(trampoline, 0, MEM_RELEASE);
        return nullptr;
    }
    FlushInstructionCache(GetCurrentProcess(), trampoline, trampoline_size);
    return trampoline;
}

bool write_detours_atomically(const std::array<HookSite, kHookCount>& sites) noexcept {
    std::array<std::array<BYTE, kMaximumPatchSize>, kHookCount> patches{};
    for (std::size_t index = 0; index < sites.size(); ++index) {
        patches[index].fill(0x90);
        if (!encode_relative(
                patches[index].data(),
                sites[index].target,
                sites[index].detour,
                sites[index].opcode)) {
            return false;
        }
    }

    std::array<DWORD, kHookCount> previous{};
    std::size_t protected_count = 0;
    for (; protected_count < sites.size(); ++protected_count) {
        if (VirtualProtect(
                sites[protected_count].target,
                sites[protected_count].patch_size,
                PAGE_EXECUTE_READWRITE,
                &previous[protected_count]) == FALSE) {
            for (std::size_t rollback = protected_count; rollback > 0; --rollback) {
                DWORD ignored = 0;
                VirtualProtect(
                    sites[rollback - 1].target,
                    sites[rollback - 1].patch_size,
                    previous[rollback - 1],
                    &ignored);
            }
            return false;
        }
    }

    for (std::size_t index = 0; index < sites.size(); ++index) {
        std::memcpy(sites[index].target, patches[index].data(), sites[index].patch_size);
        FlushInstructionCache(GetCurrentProcess(), sites[index].target, sites[index].patch_size);
    }
    for (std::size_t index = sites.size(); index > 0; --index) {
        DWORD ignored = 0;
        VirtualProtect(
            sites[index - 1].target,
            sites[index - 1].patch_size,
            previous[index - 1],
            &ignored);
    }
    return true;
}

void resume_threads(std::vector<SuspendedThread>* threads) noexcept {
    for (auto iterator = threads->rbegin(); iterator != threads->rend(); ++iterator) {
        ResumeThread(iterator->handle);
        CloseHandle(iterator->handle);
    }
    threads->clear();
}

bool instruction_pointer_in_patch(
    const CONTEXT& context,
    const std::array<HookSite, kHookCount>& sites) noexcept {
    const std::uintptr_t instruction = static_cast<std::uintptr_t>(context.Eip);
    for (const HookSite& site : sites) {
        const std::uintptr_t begin = reinterpret_cast<std::uintptr_t>(site.target);
        if (instruction >= begin && instruction < begin + site.patch_size) {
            return true;
        }
    }
    return false;
}

bool suspend_other_threads(
    const std::array<HookSite, kHookCount>& sites,
    std::vector<SuspendedThread>* suspended,
    wchar_t* error,
    std::size_t error_capacity) noexcept {
    const DWORD process_id = GetCurrentProcessId();
    const DWORD current_thread = GetCurrentThreadId();
    const HANDLE snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
    if (snapshot == INVALID_HANDLE_VALUE) {
        set_error(error, error_capacity, L"lifecycle probe: thread snapshot failed; no hooks applied");
        return false;
    }

    std::vector<DWORD> thread_ids;
    THREADENTRY32 entry{};
    entry.dwSize = sizeof(entry);
    if (Thread32First(snapshot, &entry) != FALSE) {
        do {
            if (entry.th32OwnerProcessID == process_id && entry.th32ThreadID != current_thread) {
                thread_ids.push_back(entry.th32ThreadID);
            }
        } while (Thread32Next(snapshot, &entry) != FALSE);
    }
    CloseHandle(snapshot);

    suspended->reserve(thread_ids.size());
    for (const DWORD thread_id : thread_ids) {
        HANDLE thread = OpenThread(
            THREAD_SUSPEND_RESUME | THREAD_GET_CONTEXT | THREAD_QUERY_INFORMATION,
            FALSE,
            thread_id);
        if (thread == nullptr) {
            if (GetLastError() == ERROR_INVALID_PARAMETER) {
                continue;
            }
            set_error(error, error_capacity, L"lifecycle probe: could not open a client thread; no hooks applied");
            resume_threads(suspended);
            return false;
        }
        if (SuspendThread(thread) == static_cast<DWORD>(-1)) {
            CloseHandle(thread);
            set_error(error, error_capacity, L"lifecycle probe: could not suspend a client thread; no hooks applied");
            resume_threads(suspended);
            return false;
        }
        suspended->push_back({thread, thread_id});
    }

    for (const SuspendedThread& thread : *suspended) {
        CONTEXT context{};
        context.ContextFlags = CONTEXT_CONTROL;
        if (GetThreadContext(thread.handle, &context) == FALSE) {
            set_error(error, error_capacity, L"lifecycle probe: could not inspect a suspended thread; no hooks applied");
            resume_threads(suspended);
            return false;
        }
        if (instruction_pointer_in_patch(context, sites)) {
            set_error(error, error_capacity, L"lifecycle probe: a client thread occupied a patch site; retry after restart");
            resume_threads(suspended);
            return false;
        }
    }
    return true;
}

}  // namespace

bool p5136_xun_install_lifecycle_hooks(wchar_t* error, std::size_t error_capacity) noexcept {
    const auto image_base = reinterpret_cast<std::uintptr_t>(GetModuleHandleW(nullptr));
    if (image_base == 0) {
        set_error(error, error_capacity, L"lifecycle probe: client image base is unavailable; no hooks applied");
        return false;
    }

    g_p5136_add_protected_float = reinterpret_cast<void*>(image_base + kAddProtectedFloatRva);
    g_p5136_get_protected_float = reinterpret_cast<void*>(image_base + kGetProtectedFloatRva);
    g_p5136_set_collision_response = reinterpret_cast<void*>(
        image_base + kSetCollisionResponseRva);
    g_p5136_string_c_str = reinterpret_cast<void*>(image_base + 0x000DE560u);
    g_p5136_copy_shared_ref = reinterpret_cast<void*>(image_base + kCopySharedRefRva);
    g_p5136_v1_flat_gauge_allocator = reinterpret_cast<void*>(
        image_base + kV1FlatGaugeAllocatorRva);
    g_p5136_flat_gauge_config_ctor = reinterpret_cast<void*>(
        image_base + kFlatGaugeConfigCtorRva);
    g_p5136_wide_string_assign = reinterpret_cast<void*>(image_base + kWideStringAssignRva);
    g_p5136_flat_gauge_bind = reinterpret_cast<void*>(image_base + kFlatGaugeBindRva);
    g_p5136_flat_gauge_config_dtor = reinterpret_cast<void*>(
        image_base + kFlatGaugeConfigDtorRva);
    g_p5136_shared_ref_get = reinterpret_cast<void*>(image_base + kSharedRefGetRva);
    g_p5136_rect_copy = reinterpret_cast<void*>(image_base + kRectCopyRva);
    g_p5136_rect_width = reinterpret_cast<void*>(image_base + kRectWidthRva);
    g_p5136_set_panel_uv_rect = reinterpret_cast<void*>(image_base + kSetPanelUvRectRva);
    g_p5136_display_protected_float = reinterpret_cast<void*>(
        image_base + kDisplayProtectedFloatRva);
    p5136_xun_configure_charger_visual(image_base);

    std::array<HookSite, kHookCount> sites = {{
        {
            reinterpret_cast<BYTE*>(image_base + kV1TachoAllocatorRva),
            reinterpret_cast<const void*>(&p5136_xun_v1_tacho_probe),
            &g_p5136_xun_v1_tacho_trampoline,
            kExpectedTachoPrologue.data(),
            kExpectedTachoPrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kXTachoAllocatorRva),
            reinterpret_cast<const void*>(&p5136_xun_x_tacho_probe),
            &g_p5136_xun_x_tacho_trampoline,
            kExpectedTachoPrologue.data(),
            kExpectedTachoPrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kTachoFactoryLookupRva),
            reinterpret_cast<const void*>(&p5136_xun_tacho_factory_lookup_probe),
            &g_p5136_xun_tacho_factory_lookup_trampoline,
            kExpectedFactoryLookupPrologue.data(),
            kExpectedFactoryLookupPrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kV1TachoUpdateRva),
            reinterpret_cast<const void*>(&p5136_xun_v1_tacho_update_probe),
            &g_p5136_xun_v1_tacho_update_trampoline,
            kExpectedV1TachoUpdatePrologue.data(),
            kExpectedV1TachoUpdatePrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kDisplayStatRva),
            reinterpret_cast<const void*>(&p5136_xun_display_stat_probe),
            &g_p5136_xun_display_stat_trampoline,
            kExpectedDisplayStatPrologue.data(),
            kExpectedDisplayStatPrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kDriveEventRva),
            reinterpret_cast<const void*>(&p5136_xun_drive_event_probe),
            &g_p5136_xun_drive_event_trampoline,
            kExpectedAcceptedDriveEventSite.data(),
            kExpectedAcceptedDriveEventSite.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kPhysicsTickRva),
            reinterpret_cast<const void*>(&p5136_xun_physics_tick_probe),
            &g_p5136_xun_physics_tick_trampoline,
            kExpectedPhysicsTickPrologue.data(),
            kExpectedPhysicsTickPrologue.size(),
        },
        {
            reinterpret_cast<BYTE*>(image_base + kSpeedBoostGaugeAddCallRvas[0]),
            reinterpret_cast<const void*>(&p5136_xun_speed_boost_gauge_add_probe),
            nullptr,
            kExpectedSpeedBoostGaugeAddCalls[0].data(),
            kExpectedSpeedBoostGaugeAddCalls[0].size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kSpeedBoostGaugeAddCallRvas[1]),
            reinterpret_cast<const void*>(&p5136_xun_speed_boost_gauge_add_probe),
            nullptr,
            kExpectedSpeedBoostGaugeAddCalls[1].data(),
            kExpectedSpeedBoostGaugeAddCalls[1].size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kSpeedBoostGaugeAddCallRvas[2]),
            reinterpret_cast<const void*>(&p5136_xun_speed_boost_gauge_add_probe),
            nullptr,
            kExpectedSpeedBoostGaugeAddCalls[2].data(),
            kExpectedSpeedBoostGaugeAddCalls[2].size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kDriftGaugeCallRva),
            reinterpret_cast<const void*>(&p5136_xun_drift_gauge_probe),
            nullptr,
            kExpectedDriftGaugeCall.data(),
            kExpectedDriftGaugeCall.size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kCollisionResponseCallRva),
            reinterpret_cast<const void*>(&p5136_xun_collision_response_probe),
            nullptr,
            kExpectedCollisionResponseCall.data(),
            kExpectedCollisionResponseCall.size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kAntiCollideCallRvas[0]),
            reinterpret_cast<const void*>(&p5136_xun_anti_collide_probe),
            nullptr,
            kExpectedAntiCollideCalls[0].data(),
            kExpectedAntiCollideCalls[0].size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kAntiCollideCallRvas[1]),
            reinterpret_cast<const void*>(&p5136_xun_anti_collide_probe),
            nullptr,
            kExpectedAntiCollideCalls[1].data(),
            kExpectedAntiCollideCalls[1].size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kWallGaugeCallRva),
            reinterpret_cast<const void*>(&p5136_xun_wall_gauge_probe),
            nullptr,
            kExpectedWallGaugeCall.data(),
            kExpectedWallGaugeCall.size(),
            0xE8,
        },
        {
            reinterpret_cast<BYTE*>(image_base + kBoostGaugeCallRva),
            reinterpret_cast<const void*>(&p5136_xun_boost_gauge_probe),
            nullptr,
            kExpectedBoostGaugeCall.data(),
            kExpectedBoostGaugeCall.size(),
            0xE8,
        },
    }};

    for (const HookSite& site : sites) {
        if (std::memcmp(site.target, site.expected, site.patch_size) != 0) {
            set_error(error, error_capacity, L"lifecycle probe: target prologue mismatch; no hooks applied");
            return false;
        }
        if (site.trampoline_slot != nullptr) {
            *site.trampoline_slot = create_trampoline(site);
        }
        if (site.trampoline_slot != nullptr && *site.trampoline_slot == nullptr) {
            for (const HookSite& cleanup : sites) {
                if (cleanup.trampoline_slot != nullptr && *cleanup.trampoline_slot != nullptr) {
                    VirtualFree(*cleanup.trampoline_slot, 0, MEM_RELEASE);
                    *cleanup.trampoline_slot = nullptr;
                }
            }
            set_error(error, error_capacity, L"lifecycle probe: trampoline allocation failed; no hooks applied");
            return false;
        }
    }

    std::vector<SuspendedThread> suspended;
    if (!suspend_other_threads(sites, &suspended, error, error_capacity)) {
        for (const HookSite& cleanup : sites) {
            if (cleanup.trampoline_slot != nullptr && *cleanup.trampoline_slot != nullptr) {
                VirtualFree(*cleanup.trampoline_slot, 0, MEM_RELEASE);
                *cleanup.trampoline_slot = nullptr;
            }
        }
        return false;
    }

    bool success = true;
    for (const HookSite& site : sites) {
        success = success && std::memcmp(site.target, site.expected, site.patch_size) == 0;
    }
    if (success) {
        success = write_detours_atomically(sites);
    }
    resume_threads(&suspended);

    if (!success) {
        for (const HookSite& cleanup : sites) {
            if (cleanup.trampoline_slot != nullptr && *cleanup.trampoline_slot != nullptr) {
                VirtualFree(*cleanup.trampoline_slot, 0, MEM_RELEASE);
                *cleanup.trampoline_slot = nullptr;
            }
        }
        set_error(error, error_capacity, L"lifecycle probe: atomic patch installation failed and was rolled back");
        return false;
    }
    return true;
}
