#include "xun_physics_state.hpp"

#include <cmath>
#include <limits>

namespace p5136::xun {
namespace {

ChargerTransition set_active(
    ChargerState* state,
    const ChargerTuning& tuning,
    std::uint32_t now_ms,
    bool active) noexcept {
    if (!tuning.enabled || state->active == active) {
        return ChargerTransition::none;
    }
    if (active && state->pending_booster_uses != tuning.booster_use_count) {
        return ChargerTransition::none;
    }

    state->active = active;
    state->pending_booster_uses = 0;
    if (active) {
        ++state->activation_count;
        state->active_until_ms = now_ms + tuning.use_time_ms;
        return ChargerTransition::activated;
    }
    state->active_until_ms = 0;
    return ChargerTransition::deactivated;
}

}  // namespace

float scaled_speed_boost_gauge_addend(
    float original_addend,
    float multiplier) noexcept {
    if (!std::isfinite(original_addend)
        || !std::isfinite(multiplier)
        || multiplier <= 0.0F
        || multiplier == 1.0F) {
        return original_addend;
    }
    const float scaled = original_addend * multiplier;
    return std::isfinite(scaled) ? scaled : original_addend;
}

float added_charger_value(float original, float added) noexcept {
    if (!std::isfinite(original) || !std::isfinite(added) || added <= 0.0F) {
        return original;
    }
    const float result = original + added;
    return std::isfinite(result) ? result : original;
}

float selected_charger_value(float original, float replacement) noexcept {
    return std::isfinite(replacement) && replacement != 0.0F ? replacement : original;
}

std::int32_t display_part_value(std::uint8_t part_type) noexcept {
    if (part_type == 0) {
        return 0;
    }
    const std::int32_t zero_based = static_cast<std::int32_t>(part_type) - 1;
    const std::int32_t full_cycles = zero_based / 10;
    const std::int32_t position = zero_based % 10;
    std::int32_t value = 201 + full_cycles * 23;
    for (std::int32_t index = 1; index <= position; ++index) {
        if (index <= 2) {
            value += 2;
        } else if (index <= 5) {
            value += 3;
        } else if (index <= 8) {
            value += 4;
        } else {
            value += 5;
        }
    }
    return value;
}

bool corrected_display_value(
    std::uint32_t category,
    float body_value,
    std::uint8_t part_type,
    std::int16_t* output) noexcept {
    if (output == nullptr || category >= 4 || !std::isfinite(body_value)) {
        return false;
    }
    const auto round_half_away_from_zero = [](float value) noexcept {
        return value >= 0.0F
            ? static_cast<std::int32_t>(value + 0.5F)
            : static_cast<std::int32_t>(value - 0.5F);
    };
    std::int32_t body_display = 0;
    switch (category) {
        case 0:
            body_display = round_half_away_from_zero(
                (body_value - 1.4055F) * 25'000.0F + 800.0F);
            break;
        case 1:
            body_display = static_cast<std::int32_t>(body_value / 2.0F);
            break;
        case 2:
            body_display = round_half_away_from_zero((body_value + 0.5F) * 250.0F);
            break;
        case 3:
            body_display = static_cast<std::int32_t>(body_value + 540.0F + 140.0F);
            break;
        default:
            return false;
    }
    const std::int32_t combined = body_display + display_part_value(part_type);
    if (combined < std::numeric_limits<std::int16_t>::min()
        || combined > std::numeric_limits<std::int16_t>::max()) {
        return false;
    }
    *output = static_cast<std::int16_t>(combined);
    return true;
}

bool should_count_booster_event(
    std::uint32_t event_kind,
    bool accepted,
    std::uint32_t client_booster_uses_before,
    std::uint32_t client_booster_uses_after) noexcept {
    return accepted
        && (event_kind == 3u || event_kind == 4u)
        && client_booster_uses_after == client_booster_uses_before + 1u;
}

bool client_booster_counter_rewound(
    bool previously_observed,
    std::uint32_t previous,
    std::uint32_t current) noexcept {
    return previously_observed && current < previous;
}

ChargerTransition observe_drive_event(
    ChargerState* state,
    const ChargerTuning& tuning,
    std::uint32_t now_ms,
    std::uint8_t event_kind,
    bool excluded_state) noexcept {
    if (state == nullptr) {
        return ChargerTransition::none;
    }

    if (!excluded_state && (event_kind == 3u || event_kind == 4u)) {
        ++state->total_booster_uses;
        if (tuning.enabled && !state->active) {
            ++state->pending_booster_uses;
        }
    }

    if (!tuning.enabled || tuning.booster_use_count == 0 || state->active) {
        return ChargerTransition::none;
    }
    const std::uint32_t completed_cycles =
        state->total_booster_uses / tuning.booster_use_count;
    if (completed_cycles <= state->activation_count || state->active_until_ms > now_ms) {
        return ChargerTransition::none;
    }
    return set_active(state, tuning, now_ms, true);
}

ChargerTransition tick_charger(
    ChargerState* state,
    const ChargerTuning& tuning,
    std::uint32_t now_ms) noexcept {
    if (state == nullptr || !tuning.enabled || !state->active) {
        return ChargerTransition::none;
    }
    if (state->active_until_ms != 0 && now_ms > state->active_until_ms) {
        return set_active(state, tuning, now_ms, false);
    }
    return ChargerTransition::none;
}

void reset_charger(ChargerState* state) noexcept {
    if (state != nullptr) {
        *state = {};
    }
}

}  // namespace p5136::xun
