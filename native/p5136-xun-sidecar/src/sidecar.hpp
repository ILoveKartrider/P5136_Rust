#pragma once

#include <windows.h>

#include "p5136_xun_api.h"

void p5136_xun_set_module(HMODULE module) noexcept;
void p5136_xun_set_proxy_ready(bool ready) noexcept;
void p5136_xun_initialize() noexcept;
P5136XunStatusSnapshot p5136_xun_snapshot() noexcept;
void p5136_xun_log_charger_transition(
    const void* kart,
    bool active,
    uint32_t activation_count,
    uint32_t total_booster_uses,
    uint32_t active_until_ms) noexcept;
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
    bool charger_active) noexcept;
void p5136_xun_log_physics_tick_sample(
    const void* kart,
    uint32_t client_time_ms,
    uint32_t tick_calls,
    uint32_t total_booster_uses,
    uint32_t pending_booster_uses,
    bool charger_active,
    uint32_t active_until_ms) noexcept;
void p5136_xun_log_speed_boost_gauge_adjustment(
    const void* kart,
    uint32_t adjustment_count,
    float original_addend,
    float scaled_addend,
    float multiplier) noexcept;
void p5136_xun_log_charger_visual(
    const void* kart,
    const wchar_t* action,
    const void* effect) noexcept;
void p5136_xun_log_exceed_gauge(
    const void* tacho,
    const void* controller,
    float normalized_fill,
    bool feature_present,
    bool active,
    bool usable,
    uint32_t visual_state) noexcept;
void p5136_xun_log_message(const wchar_t* message) noexcept;
void p5136_xun_log_server_profile(
    uint32_t generation,
    uint16_t kart_id,
    uint8_t exceed_type,
    uint8_t profile_state,
    bool enabled,
    uint32_t booster_use_count,
    uint32_t use_time_ms) noexcept;
void p5136_xun_log_local_kart_binding(
    const void* kart,
    uint32_t generation,
    uint16_t kart_id) noexcept;
extern "C" void __cdecl p5136_xun_record_v1_tacho_allocator() noexcept;
extern "C" void __cdecl p5136_xun_record_x_tacho_allocator() noexcept;
