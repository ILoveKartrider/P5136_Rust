#pragma once

#include <cstdint>

namespace p5136::xun {

struct ChargerTuning {
    bool enabled = false;
    bool apply_speed_boost_gauge = false;
    bool apply_remaining_consumers = false;
    std::uint32_t booster_use_count = 0;
    std::uint32_t use_time_ms = 0;
    float wall_gauge_added = 0.0F;
    float boost_gauge_added = 0.0F;
    float drift_gauge_factor = 0.0F;
    float charge_boost_by_speed_multiplier = 1.0F;
    float anti_collide_balance = 0.0F;
};

float scaled_speed_boost_gauge_addend(
    float original_addend,
    float multiplier) noexcept;
float added_charger_value(float original, float added) noexcept;
float selected_charger_value(float original, float replacement) noexcept;
std::int32_t display_part_value(std::uint8_t part_type) noexcept;
bool corrected_display_value(
    std::uint32_t category,
    float body_value,
    std::uint8_t part_type,
    std::int16_t* output) noexcept;

struct ChargerState {
    bool active = false;
    std::uint32_t total_booster_uses = 0;
    std::uint32_t activation_count = 0;
    std::uint32_t pending_booster_uses = 0;
    std::uint32_t active_until_ms = 0;
};

enum class ChargerTransition : std::uint8_t {
    none = 0,
    activated = 1,
    deactivated = 2,
};

bool should_count_booster_event(
    std::uint32_t event_kind,
    bool accepted,
    std::uint32_t client_booster_uses_before,
    std::uint32_t client_booster_uses_after) noexcept;

bool client_booster_counter_rewound(
    bool previously_observed,
    std::uint32_t previous,
    std::uint32_t current) noexcept;

ChargerTransition observe_drive_event(
    ChargerState* state,
    const ChargerTuning& tuning,
    std::uint32_t now_ms,
    std::uint8_t event_kind,
    bool excluded_state) noexcept;

ChargerTransition tick_charger(
    ChargerState* state,
    const ChargerTuning& tuning,
    std::uint32_t now_ms) noexcept;

void reset_charger(ChargerState* state) noexcept;

}  // namespace p5136::xun
