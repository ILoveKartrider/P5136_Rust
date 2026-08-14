#include "xun_physics_runtime.hpp"

#include <windows.h>

#include <array>
#include <cmath>
#include <cstddef>
#include <cstring>
#include <limits>

#include "charger_visual.hpp"
#include "sidecar.hpp"
#include "xun_profile_client.hpp"

namespace {

constexpr std::size_t kMaximumTrackedKarts = 32;

struct TrackedKart {
    void* key = nullptr;
    std::uint32_t last_tick_ms = 0;
    std::uint32_t tick_calls = 0;
    std::uint32_t last_tick_log_ms = 0;
    bool saw_drive_event = false;
    bool visual_create_in_flight = false;
    bool visual_active = false;
    bool saw_client_booster_counter = false;
    std::uint32_t last_visual_create_attempt_ms = 0;
    std::uint32_t last_client_booster_uses = 0;
    void* charger_visual = nullptr;
    p5136::xun::ChargerState charger{};
};

SRWLOCK g_physics_lock = SRWLOCK_INIT;
p5136::xun::ChargerTuning g_tuning{};
std::array<TrackedKart, kMaximumTrackedKarts> g_karts{};
P5136XunPhysicsProbeSnapshot g_probe{};
void* g_local_kart = nullptr;
std::uint32_t g_profile_generation = 0;
std::uint16_t g_profile_kart_id = 0;
std::uint8_t g_profile_state = 0;
std::array<std::uint8_t, 4> g_default_part_types{};

void discard_state(TrackedKart* entry) noexcept {
    if (entry->charger.active && g_probe.active_karts != 0) {
        --g_probe.active_karts;
    }
    *entry = {};
}

TrackedKart* find_or_allocate_kart(
    void* key,
    std::uint32_t now_ms,
    bool reset_on_time_rewind) noexcept {
    TrackedKart* oldest = &g_karts.front();
    for (TrackedKart& entry : g_karts) {
        if (entry.key == key) {
            if (reset_on_time_rewind && now_ms < entry.last_tick_ms) {
                void* charger_visual = entry.charger_visual;
                const bool visual_active = entry.visual_active;
                discard_state(&entry);
                entry.key = key;
                entry.charger_visual = charger_visual;
                entry.visual_active = visual_active;
            }
            return &entry;
        }
        if (entry.key == nullptr) {
            entry = {};
            entry.key = key;
            return &entry;
        }
        if (entry.last_tick_ms < oldest->last_tick_ms) {
            oldest = &entry;
        }
    }
    discard_state(oldest);
    oldest->key = key;
    return oldest;
}

bool remaining_consumers_active_locked(void* kart) noexcept {
    if (!g_tuning.enabled
        || !g_tuning.apply_remaining_consumers
        || kart != g_local_kart) {
        return false;
    }
    for (const TrackedKart& entry : g_karts) {
        if (entry.key == kart) {
            return entry.charger.active;
        }
    }
    return false;
}

void record_transition(
    void* kart,
    const p5136::xun::ChargerState& state,
    p5136::xun::ChargerTransition transition) noexcept {
    if (transition == p5136::xun::ChargerTransition::activated) {
        ++g_probe.charger_activations;
        ++g_probe.active_karts;
    } else if (transition == p5136::xun::ChargerTransition::deactivated) {
        ++g_probe.charger_deactivations;
        if (g_probe.active_karts != 0) {
            --g_probe.active_karts;
        }
    }
    if (transition != p5136::xun::ChargerTransition::none) {
        p5136_xun_log_charger_transition(
            kart,
            state.active,
            state.activation_count,
            state.total_booster_uses,
            state.active_until_ms);
    }
}

void synchronize_charger_visual(void* kart, std::uint32_t now_ms) noexcept {
    enum class Action {
        none,
        create,
        start,
        stop,
    };
    Action action = Action::none;
    void* visual = nullptr;

    AcquireSRWLockExclusive(&g_physics_lock);
    TrackedKart* entry = nullptr;
    for (TrackedKart& candidate : g_karts) {
        if (candidate.key == kart) {
            entry = &candidate;
            break;
        }
    }
    if (entry != nullptr && kart == g_local_kart) {
        const bool should_be_active = g_tuning.enabled && entry->charger.active;
        visual = entry->charger_visual;
        if (should_be_active
            && visual == nullptr
            && !entry->visual_create_in_flight
            && (entry->last_visual_create_attempt_ms == 0
                || now_ms - entry->last_visual_create_attempt_ms >= 1000u)) {
            entry->visual_create_in_flight = true;
            entry->last_visual_create_attempt_ms = now_ms;
            action = Action::create;
            ++g_probe.charger_resource_registrations;
        } else if (visual != nullptr && should_be_active != entry->visual_active) {
            action = should_be_active ? Action::start : Action::stop;
        }
    }
    ReleaseSRWLockExclusive(&g_physics_lock);

    if (action == Action::create) {
        void* created = p5136_xun_create_charger_visual(kart);
        bool retained = false;
        AcquireSRWLockExclusive(&g_physics_lock);
        for (TrackedKart& candidate : g_karts) {
            if (candidate.key != kart) {
                continue;
            }
            candidate.visual_create_in_flight = false;
            if (created != nullptr
                && g_tuning.enabled
                && candidate.charger.active
                && candidate.charger_visual == nullptr) {
                candidate.charger_visual = created;
                visual = created;
                retained = true;
            }
            break;
        }
        ReleaseSRWLockExclusive(&g_physics_lock);
        p5136_xun_log_charger_visual(
            kart,
            retained ? L"created" : L"create-failed",
            created);
        if (!retained) {
            return;
        }
        action = Action::start;
    }

    if (action == Action::start || action == Action::stop) {
        const bool activate = action == Action::start;
        const bool changed = p5136_xun_set_charger_visual_active(
            visual,
            activate,
            now_ms);
        AcquireSRWLockExclusive(&g_physics_lock);
        for (TrackedKart& candidate : g_karts) {
            if (candidate.key != kart || candidate.charger_visual != visual) {
                continue;
            }
            if (changed) {
                candidate.visual_active = activate;
            }
            if (changed && activate) {
                ++g_probe.charger_effect_spawns;
            } else if (changed) {
                ++g_probe.charger_effect_removals;
            }
            break;
        }
        ReleaseSRWLockExclusive(&g_physics_lock);
        p5136_xun_log_charger_visual(
            kart,
            changed ? (activate ? L"started" : L"stopped") : L"state-failed",
            visual);
        if (!changed || !activate) {
            return;
        }
    }

    bool active = false;
    AcquireSRWLockShared(&g_physics_lock);
    for (const TrackedKart& candidate : g_karts) {
        if (candidate.key == kart && candidate.charger_visual == visual) {
            active = candidate.visual_active;
            break;
        }
    }
    ReleaseSRWLockShared(&g_physics_lock);
    if (active && visual != nullptr) {
        p5136_xun_tick_charger_visual(visual, now_ms);
    }
}

}  // namespace

void p5136_xun_configure_physics_probe(const p5136::xun::ChargerTuning& tuning) noexcept {
    AcquireSRWLockExclusive(&g_physics_lock);
    g_tuning = tuning;
    g_karts = {};
    g_probe = {};
    g_local_kart = nullptr;
    g_profile_generation = 0;
    g_profile_kart_id = 0;
    g_profile_state = 0;
    g_default_part_types = {};
    ReleaseSRWLockExclusive(&g_physics_lock);
}

void p5136_xun_apply_server_profile(
    const p5136::xun::ChargerTuning& tuning,
    std::uint32_t generation,
    std::uint16_t kart_id,
    std::uint8_t exceed_type,
    std::uint8_t profile_state,
    std::uint8_t default_engine_type,
    std::uint8_t default_handle_type,
    std::uint8_t default_wheel_type,
    std::uint8_t default_booster_type) noexcept {
    AcquireSRWLockExclusive(&g_physics_lock);
    g_tuning = tuning;
    // A profile frame can race GoPlayKart setup. Preserve the renderer object
    // already attached to that kart while resetting the previous generation's
    // charger state.
    std::array<TrackedKart, kMaximumTrackedKarts> reset_karts{};
    for (std::size_t index = 0; index < g_karts.size(); ++index) {
        if (g_karts[index].key != nullptr && g_karts[index].charger_visual != nullptr) {
            reset_karts[index].key = g_karts[index].key;
            reset_karts[index].charger_visual = g_karts[index].charger_visual;
            reset_karts[index].visual_active = g_karts[index].visual_active;
        }
    }
    g_karts = reset_karts;
    g_probe.active_karts = 0;
    g_local_kart = nullptr;
    g_profile_generation = generation;
    g_profile_kart_id = kart_id;
    g_profile_state = profile_state;
    g_default_part_types = {
        default_engine_type,
        default_wheel_type,
        default_handle_type,
        default_booster_type,
    };
    ReleaseSRWLockExclusive(&g_physics_lock);
    p5136_xun_log_server_profile(
        generation,
        kart_id,
        exceed_type,
        profile_state,
        tuning.enabled,
        tuning.booster_use_count,
        tuning.use_time_ms);
}

void p5136_xun_reset_server_profile() noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const bool already_disabled = !g_tuning.enabled
        && g_profile_generation == 0
        && g_profile_kart_id == 0;
    ReleaseSRWLockShared(&g_physics_lock);
    if (already_disabled) {
        return;
    }
    p5136::xun::ChargerTuning disabled{};
    p5136_xun_apply_server_profile(disabled, 0, 0, 0, 0, 0, 0, 0, 0);
}

P5136XunPhysicsProbeSnapshot p5136_xun_physics_probe_snapshot() noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const P5136XunPhysicsProbeSnapshot result = g_probe;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

bool p5136_xun_dashboard_state(
    void* kart,
    P5136XunDashboardState* output) noexcept {
    if (output == nullptr) {
        return false;
    }
    *output = {};
    AcquireSRWLockShared(&g_physics_lock);
    void* key = kart;
    if (key == nullptr || key != g_local_kart) {
        key = g_local_kart;
    }
    if (g_tuning.enabled && key != nullptr && g_tuning.booster_use_count != 0) {
        for (const TrackedKart& entry : g_karts) {
            if (entry.key != key) {
                continue;
            }
            output->enabled = true;
            output->active = entry.charger.active;
            output->pending_booster_uses = entry.charger.pending_booster_uses;
            output->required_booster_uses = g_tuning.booster_use_count;
            output->active_until_ms = entry.charger.active_until_ms;
            output->active_duration_ms = g_tuning.use_time_ms;
            break;
        }
    }
    ReleaseSRWLockShared(&g_physics_lock);
    return output->enabled;
}

bool p5136_xun_correct_display_stat(
    std::uint32_t category,
    float body_value,
    std::int16_t* output) noexcept {
    if (output == nullptr || category >= 4) {
        return false;
    }
    std::uint8_t part_type = 0;
    bool enabled = false;
    AcquireSRWLockShared(&g_physics_lock);
    enabled = g_profile_kart_id != 0 && g_profile_state != 0;
    if (enabled) {
        part_type = g_default_part_types[category];
    }
    ReleaseSRWLockShared(&g_physics_lock);
    if (!enabled) {
        return false;
    }

    return p5136::xun::corrected_display_value(category, body_value, part_type, output);
}

extern "C" void __cdecl p5136_xun_observe_drive_event(
    void* kart,
    std::uint32_t event_payload,
    std::uint32_t event_kind,
    std::uint32_t accepted,
    std::uint32_t client_booster_uses_before,
    std::uint32_t client_booster_uses_after) noexcept {
    if (kart == nullptr) {
        return;
    }
    AcquireSRWLockExclusive(&g_physics_lock);
    ++g_probe.drive_event_calls;
    TrackedKart* entry = find_or_allocate_kart(kart, 0, false);
    entry->saw_drive_event = true;
    const bool race_reset = g_tuning.enabled
        && kart == g_local_kart
        && p5136::xun::client_booster_counter_rewound(
            entry->saw_client_booster_counter,
            entry->last_client_booster_uses,
            client_booster_uses_after);
    entry->saw_client_booster_counter = true;
    entry->last_client_booster_uses = client_booster_uses_after;
    if (race_reset) {
        if (entry->charger.active && g_probe.active_karts != 0) {
            --g_probe.active_karts;
        }
        p5136::xun::reset_charger(&entry->charger);
        p5136_xun_log_message(
            L"race boundary: reset charger state after the client booster counter rewound");
    }
    const bool is_booster_event = event_kind == 3u || event_kind == 4u;
    const bool counted_as_booster = p5136::xun::should_count_booster_event(
        event_kind,
        accepted != 0,
        client_booster_uses_before,
        client_booster_uses_after);
    if (counted_as_booster && g_tuning.enabled && g_local_kart == nullptr) {
        g_local_kart = kart;
        p5136_xun_log_local_kart_binding(
            kart,
            g_profile_generation,
            g_profile_kart_id);
    }
    if (kart != g_local_kart) {
        ReleaseSRWLockExclusive(&g_physics_lock);
        return;
    }
    const p5136::xun::ChargerTransition transition = p5136::xun::observe_drive_event(
        &entry->charger,
        g_tuning,
        entry->last_tick_ms,
        static_cast<std::uint8_t>(event_kind),
        !counted_as_booster);
    const std::uint32_t event_calls = g_probe.drive_event_calls;
    const bool sparse_sample = event_calls <= 16u || (event_calls & (event_calls - 1u)) == 0u;
    if (is_booster_event || sparse_sample) {
        p5136_xun_log_drive_event_sample(
            kart,
            event_payload,
            event_kind,
            accepted != 0,
            counted_as_booster,
            client_booster_uses_before,
            client_booster_uses_after,
            entry->last_tick_ms,
            entry->charger.pending_booster_uses,
            entry->charger.active);
    }
    record_transition(kart, entry->charger, transition);
    ReleaseSRWLockExclusive(&g_physics_lock);
    if (race_reset) {
        p5136_xun_request_race_reset();
    }
}

extern "C" void __cdecl p5136_xun_observe_physics_tick(
    void* kart,
    std::uint32_t now_ms) noexcept {
    if (kart == nullptr) {
        return;
    }
    AcquireSRWLockExclusive(&g_physics_lock);
    ++g_probe.physics_tick_calls;
    TrackedKart* entry = find_or_allocate_kart(kart, now_ms, true);
    entry->last_tick_ms = now_ms;
    ++entry->tick_calls;
    if (g_local_kart != nullptr && kart != g_local_kart) {
        ReleaseSRWLockExclusive(&g_physics_lock);
        return;
    }
    const p5136::xun::ChargerTransition transition =
        p5136::xun::tick_charger(&entry->charger, g_tuning, now_ms);
    record_transition(kart, entry->charger, transition);
    const bool first_tick = entry->tick_calls == 1u;
    const bool due_after_drive_event = entry->saw_drive_event
        && (entry->last_tick_log_ms == 0u || now_ms - entry->last_tick_log_ms >= 1000u);
    if (first_tick || due_after_drive_event) {
        p5136_xun_log_physics_tick_sample(
            kart,
            now_ms,
            entry->tick_calls,
            entry->charger.total_booster_uses,
            entry->charger.pending_booster_uses,
            entry->charger.active,
            entry->charger.active_until_ms);
        entry->last_tick_log_ms = now_ms;
    }
    ReleaseSRWLockExclusive(&g_physics_lock);
    synchronize_charger_visual(kart, now_ms);
}

float p5136_xun_speed_boost_gauge_multiplier(void* kart) noexcept {
    if (kart == nullptr) {
        return 1.0F;
    }
    float multiplier = 1.0F;
    AcquireSRWLockShared(&g_physics_lock);
    if (g_tuning.enabled
        && g_tuning.apply_speed_boost_gauge
        && kart == g_local_kart) {
        for (const TrackedKart& entry : g_karts) {
            if (entry.key == kart && entry.charger.active) {
                multiplier = g_tuning.charge_boost_by_speed_multiplier;
                break;
            }
        }
    }
    ReleaseSRWLockShared(&g_physics_lock);
    return multiplier;
}

float p5136_xun_drift_gauge_coefficient(void* kart, float original) noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const float result = remaining_consumers_active_locked(kart)
        ? p5136::xun::added_charger_value(original, g_tuning.drift_gauge_factor)
        : original;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

float p5136_xun_wall_gauge_coefficient(void* kart, float original) noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const float result = remaining_consumers_active_locked(kart)
        ? p5136::xun::added_charger_value(original, g_tuning.wall_gauge_added)
        : original;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

float p5136_xun_boost_gauge_coefficient(void* kart, float original) noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const float result = remaining_consumers_active_locked(kart)
        ? p5136::xun::added_charger_value(original, g_tuning.boost_gauge_added)
        : original;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

float p5136_xun_anti_collide_balance(void* kart, float original) noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const float result = remaining_consumers_active_locked(kart)
        ? p5136::xun::selected_charger_value(original, g_tuning.anti_collide_balance)
        : original;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

float p5136_xun_collision_response_value(void* kart, float original) noexcept {
    AcquireSRWLockShared(&g_physics_lock);
    const float result = remaining_consumers_active_locked(kart)
        ? p5136::xun::selected_charger_value(original, 100.0F)
        : original;
    ReleaseSRWLockShared(&g_physics_lock);
    return result;
}

void p5136_xun_record_speed_boost_gauge_adjustment(
    void* kart,
    float original_addend,
    float scaled_addend,
    float multiplier) noexcept {
    AcquireSRWLockExclusive(&g_physics_lock);
    const std::uint32_t count = ++g_probe.speed_boost_gauge_adjustments;
    ReleaseSRWLockExclusive(&g_physics_lock);
    if (count <= 8u || (count & (count - 1u)) == 0u) {
        p5136_xun_log_speed_boost_gauge_adjustment(
            kart,
            count,
            original_addend,
            scaled_addend,
            multiplier);
    }
}

extern "C" std::uint32_t __cdecl p5136_xun_adjust_speed_boost_gauge_addend(
    void* protected_accumulator,
    std::uint32_t addend_bits) noexcept {
    float original_addend = 0.0F;
    std::memcpy(&original_addend, &addend_bits, sizeof(original_addend));
    if (protected_accumulator == nullptr) {
        return addend_bits;
    }
    auto* kart = static_cast<std::uint8_t*>(protected_accumulator) - 0x0AACu;
    const float multiplier = p5136_xun_speed_boost_gauge_multiplier(kart);
    const float scaled_addend = p5136::xun::scaled_speed_boost_gauge_addend(
        original_addend,
        multiplier);
    if (scaled_addend != original_addend) {
        p5136_xun_record_speed_boost_gauge_adjustment(
            kart,
            original_addend,
            scaled_addend,
            multiplier);
    }
    std::uint32_t scaled_bits = 0;
    std::memcpy(&scaled_bits, &scaled_addend, sizeof(scaled_bits));
    return scaled_bits;
}
