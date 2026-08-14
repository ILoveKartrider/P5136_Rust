#pragma once

#include <stdint.h>

#if defined(_WIN32)
#define P5136_XUN_CALL __stdcall
#else
#define P5136_XUN_CALL
#endif

#define P5136_XUN_ABI_VERSION 11u

enum P5136XunStatus : uint32_t {
    P5136_XUN_STATUS_UNINITIALIZED = 0,
    P5136_XUN_STATUS_UNSUPPORTED_PROCESS = 1,
    P5136_XUN_STATUS_EXACT_BUILD_DISABLED = 2,
    P5136_XUN_STATUS_DIAGNOSTIC_READY = 3,
    P5136_XUN_STATUS_INITIALIZATION_FAILED = 4,
    P5136_XUN_STATUS_LIFECYCLE_PROBE_READY = 5,
    P5136_XUN_STATUS_XUN_TACHO_ALIAS_READY = 6,
};

enum P5136XunFlags : uint32_t {
    P5136_XUN_FLAG_PROXY_READY = 1u << 0,
    P5136_XUN_FLAG_EXACT_P5136_BUILD = 1u << 1,
    P5136_XUN_FLAG_CONFIG_ENABLED = 1u << 2,
    P5136_XUN_FLAG_HOOKS_INSTALLED = 1u << 3,
    P5136_XUN_FLAG_XUN_TACHO_ALIAS_REGISTERED = 1u << 4,
    P5136_XUN_FLAG_PHYSICS_STATE_PROBE_INSTALLED = 1u << 5,
    P5136_XUN_FLAG_SPEED_BOOST_GAUGE_CONSUMER_ENABLED = 1u << 6,
    P5136_XUN_FLAG_REMAINING_PHYSICS_CONSUMERS_ENABLED = 1u << 7,
    P5136_XUN_FLAG_CHARGER_VISUAL_ENABLED = 1u << 8,
};

struct P5136XunStatusSnapshot {
    uint32_t size;
    uint32_t abi_version;
    uint32_t status;
    uint32_t flags;
    uint32_t process_id;
    uint32_t image_timestamp;
    uint32_t image_size;
    uint32_t entry_point_rva;
    uint8_t image_sha256[32];
    uint32_t v1_tacho_allocator_calls;
    uint32_t x_tacho_allocator_calls;
    uint32_t v1_tacho_last_thread_id;
    uint32_t x_tacho_last_thread_id;
    uint32_t xun_tacho_registration_id;
    uint32_t drive_event_calls;
    uint32_t physics_tick_calls;
    uint32_t charger_activations;
    uint32_t charger_deactivations;
    uint32_t active_charger_karts;
    uint32_t speed_boost_gauge_adjustments;
    uint32_t charger_resource_registrations;
    uint32_t charger_effect_spawns;
    uint32_t charger_effect_removals;
};

#ifdef __cplusplus
extern "C" {
#endif

uint32_t P5136_XUN_CALL P5136XunGetAbiVersion(void);
uint32_t P5136_XUN_CALL P5136XunInitialize(void* reserved);
int32_t P5136_XUN_CALL P5136XunGetStatus(struct P5136XunStatusSnapshot* output);
int32_t P5136_XUN_CALL P5136XunVerifyExecutableFile(const wchar_t* path);

#ifdef __cplusplus
}
#endif
