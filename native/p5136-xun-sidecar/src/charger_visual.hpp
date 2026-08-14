#pragma once

#include <cstdint>

// P5136 has the renderer/resource ABI required by the modern charger aura,
// but predates the ReChargerEffect wrapper class. These functions own a
// sidecar ReCrashEffect instance whose attached resource is replaced with the
// imported charger scene.
void p5136_xun_configure_charger_visual(std::uintptr_t image_base) noexcept;
void* p5136_xun_create_charger_visual(void* kart) noexcept;
bool p5136_xun_set_charger_visual_active(
    void* effect,
    bool active,
    std::uint32_t now_ms) noexcept;
void p5136_xun_tick_charger_visual(void* effect, std::uint32_t now_ms) noexcept;
