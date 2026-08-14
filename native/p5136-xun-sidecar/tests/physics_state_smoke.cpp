#include "xun_physics_state.hpp"

#include <cmath>
#include <cstdio>

namespace {

bool expect(bool condition, const char* message) {
    if (!condition) {
        std::fprintf(stderr, "%s\n", message);
    }
    return condition;
}

}  // namespace

int main() {
    p5136::xun::ChargerTuning tuning{};
    tuning.enabled = true;
    tuning.booster_use_count = 3;
    tuning.use_time_ms = 1000;
    p5136::xun::ChargerState state{};

    using p5136::xun::ChargerTransition;
    bool ok = true;
    ok &= expect(
        std::fabs(p5136::xun::scaled_speed_boost_gauge_addend(0.002F, 350.0F) - 0.7F)
            < 0.000001F,
        "speed-based boost-gauge multiplier was not applied");
    ok &= expect(
        p5136::xun::scaled_speed_boost_gauge_addend(0.002F, 1.0F) == 0.002F,
        "disabled boost-gauge multiplier changed the addend");
    ok &= expect(
        std::fabs(p5136::xun::added_charger_value(1.25F, 2.0F) - 3.25F) < 0.000001F,
        "drift-gauge factor addition mismatch");
    ok &= expect(
        std::fabs(p5136::xun::added_charger_value(0.04F, 0.09F) - 0.13F) < 0.000001F,
        "wall-gauge addition mismatch");
    ok &= expect(
        std::fabs(p5136::xun::added_charger_value(0.04F, 0.03F) - 0.07F) < 0.000001F,
        "boost-gauge addition mismatch");
    ok &= expect(
        p5136::xun::selected_charger_value(0.35F, 0.8F) == 0.8F,
        "anti-collision balance selection mismatch");
    ok &= expect(
        p5136::xun::selected_charger_value(0.35F, 100.0F) == 100.0F,
        "collision-response selection mismatch");
    std::int16_t display = 0;
    ok &= expect(
        p5136::xun::corrected_display_value(0, 1.41F, 21, &display) && display == 1159,
        "Black Knight XUN acceleration display conversion mismatch");
    ok &= expect(
        p5136::xun::corrected_display_value(1, 1823.0F, 21, &display) && display == 1158,
        "Black Knight XUN drift display conversion mismatch");
    ok &= expect(
        p5136::xun::corrected_display_value(2, 2.71F, 21, &display) && display == 1050,
        "Black Knight XUN corner display conversion mismatch");
    ok &= expect(
        p5136::xun::corrected_display_value(3, 127.0F, 21, &display) && display == 1054,
        "Black Knight XUN booster-time display conversion mismatch");
    ok &= expect(
        p5136::xun::should_count_booster_event(3, true, 4, 5),
        "accepted kind-3 event with a client counter increment was not counted");
    ok &= expect(
        p5136::xun::should_count_booster_event(4, true, 0xFFFFFFFFu, 0),
        "client booster counter wraparound was not counted");
    ok &= expect(
        !p5136::xun::should_count_booster_event(3, false, 4, 5),
        "rejected event was counted");
    ok &= expect(
        !p5136::xun::should_count_booster_event(3, true, 4, 4),
        "event without a client counter increment was counted");
    ok &= expect(
        !p5136::xun::should_count_booster_event(2, true, 4, 5),
        "non-booster event was counted");
    ok &= expect(
        !p5136::xun::client_booster_counter_rewound(false, 4, 0),
        "an uninitialized counter was mistaken for a race boundary");
    ok &= expect(
        !p5136::xun::client_booster_counter_rewound(true, 4, 4),
        "a stable counter was mistaken for a race boundary");
    ok &= expect(
        p5136::xun::client_booster_counter_rewound(true, 4, 0),
        "a new-race client counter rewind was not detected");
    ok &= expect(
        p5136::xun::observe_drive_event(&state, tuning, 100, 3, false)
            == ChargerTransition::none,
        "first booster must not activate the charger");
    ok &= expect(
        p5136::xun::observe_drive_event(&state, tuning, 100, 4, false)
            == ChargerTransition::none,
        "second booster must not activate the charger");
    ok &= expect(
        p5136::xun::observe_drive_event(&state, tuning, 100, 3, false)
            == ChargerTransition::activated,
        "third booster must activate the charger");
    ok &= expect(state.active && state.active_until_ms == 1100, "activation deadline mismatch");
    ok &= expect(
        p5136::xun::tick_charger(&state, tuning, 1100) == ChargerTransition::none,
        "the later client uses a strict deadline comparison");
    ok &= expect(
        p5136::xun::tick_charger(&state, tuning, 1101) == ChargerTransition::deactivated,
        "charger did not deactivate after its deadline");

    p5136::xun::observe_drive_event(&state, tuning, 1200, 3, true);
    ok &= expect(state.total_booster_uses == 3, "excluded state incorrectly counted a booster");
    p5136::xun::observe_drive_event(&state, tuning, 1200, 3, false);
    p5136::xun::observe_drive_event(&state, tuning, 1200, 4, false);
    ok &= expect(
        p5136::xun::observe_drive_event(&state, tuning, 1200, 3, false)
            == ChargerTransition::activated,
        "second complete booster cycle did not reactivate the charger");
    ok &= expect(state.activation_count == 2, "activation count mismatch");

    p5136::xun::reset_charger(&state);
    tuning.enabled = false;
    p5136::xun::observe_drive_event(&state, tuning, 1, 3, false);
    ok &= expect(
        state.total_booster_uses == 1 && !state.active && state.pending_booster_uses == 0,
        "disabled charger state did not preserve the later-client counter boundary");
    return ok ? 0 : 1;
}
