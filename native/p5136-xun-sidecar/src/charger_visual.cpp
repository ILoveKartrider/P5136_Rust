#include "charger_visual.hpp"

#include <windows.h>

#include <array>
#include <cstddef>
#include <cstdint>

namespace {

constexpr std::size_t kCrashEffectSize = 0x19Cu;
constexpr std::size_t kAttachedResourceOffset = 0x194u;

constexpr std::uintptr_t kWideStringCtorRva = 0x000DDAE0u;
constexpr std::uintptr_t kWideStringDtorRva = 0x000DDF80u;
constexpr std::uintptr_t kSharedValueDtorRva = 0x000DE050u;
constexpr std::uintptr_t kResourceCatalogRva = 0x00DF9540u;
constexpr std::uintptr_t kResourceCatalogLookupRva = 0x000DE5D0u;
constexpr std::uintptr_t kResourceContextResolveRva = 0x004D9C90u;
constexpr std::uintptr_t kResourcePathComposeRva = 0x00AF7280u;
constexpr std::uintptr_t kResourceAssignRva = 0x001445C0u;
constexpr std::uintptr_t kCrashEffectCtorRva = 0x00A5DCA0u;
constexpr std::uintptr_t kCrashEffectAttachRva = 0x00A5DD60u;
constexpr std::uintptr_t kStartResourceChildrenRva = 0x00AEBFE0u;
constexpr std::uintptr_t kStopResourceChildrenRva = 0x00AEC120u;
constexpr std::uintptr_t kFindFirstBillboardRva = 0x00B2A150u;
constexpr std::uintptr_t kSetRendereeDepthRva = 0x00B2A530u;
constexpr std::uintptr_t kRendererPropertyRva = 0x00E0522Cu;
constexpr std::uintptr_t kClientOperatorNewRva = 0x00BE4435u;
constexpr std::uintptr_t kAlphaPropertyCtorRva = 0x00AE5420u;

constexpr std::size_t kElementChildrenBeginOffset = 0x10u;
constexpr std::size_t kElementChildrenEndOffset = 0x14u;
constexpr std::size_t kRendereeAlphaPropertyOffset = 0xA8u;
constexpr std::size_t kRendereeRendererPropertyOffset = 0xC8u;
constexpr std::size_t kRendereeParentOffset = 0xD0u;
constexpr std::size_t kRendereeDirtyOffset = 0x17Du;
constexpr std::size_t kRendereeAncestorDirtyOffset = 0x17Eu;
constexpr std::uint32_t kAuraRenderDepthBits = 0xC47A0000u;  // -1000.0F
constexpr std::size_t kMaximumSceneNodes = 4096u;

using WideStringCtor = void*(__thiscall*)(void*, const wchar_t*);
using WideStringDtor = void(__thiscall*)(void*);
using SharedValueDtor = void(__thiscall*)(void*);
using ResourceAssign = void*(__thiscall*)(void*, const void*);
using CrashEffectCtor = void*(__thiscall*)(void*);
using CrashEffectAttach = void(__thiscall*)(void*, void*);
using SetEffectActive = std::int32_t(__thiscall*)(void*, std::uint8_t, std::uint8_t);
using StartResourceChildren = void(__thiscall*)(void*, std::uint32_t, std::uint32_t);
using StopResourceChildren = void(__thiscall*)(void*);
using FindFirstBillboard = void*(__cdecl*)(void*);
using SetRendereeDepth = bool(__cdecl*)(void*, std::uint32_t);
using ClientOperatorNew = void*(__cdecl*)(std::size_t);
using AlphaPropertyCtor = void*(__thiscall*)(void*);
using TickEffect = void(__thiscall*)(void*, std::uint32_t);

std::uintptr_t g_image_base = 0;
void* g_resource_catalog = nullptr;
void* g_resource_catalog_lookup = nullptr;
void* g_resource_context_resolve = nullptr;
void* g_resource_path_compose = nullptr;
WideStringCtor g_wide_string_ctor = nullptr;
WideStringDtor g_wide_string_dtor = nullptr;
SharedValueDtor g_shared_value_dtor = nullptr;
ResourceAssign g_resource_assign = nullptr;
CrashEffectCtor g_crash_effect_ctor = nullptr;
CrashEffectAttach g_crash_effect_attach = nullptr;
StartResourceChildren g_start_resource_children = nullptr;
StopResourceChildren g_stop_resource_children = nullptr;
FindFirstBillboard g_find_first_billboard = nullptr;
SetRendereeDepth g_set_renderee_depth = nullptr;
ClientOperatorNew g_client_operator_new = nullptr;
AlphaPropertyCtor g_alpha_property_ctor = nullptr;
void** g_renderer_property = nullptr;

void* attached_resource(void* effect) noexcept {
    if (effect == nullptr) {
        return nullptr;
    }
    return *reinterpret_cast<void**>(
        static_cast<std::uint8_t*>(effect) + kAttachedResourceOffset);
}

void release_client_reference(void* value) noexcept {
    if (value == nullptr) {
        return;
    }
    auto* references = reinterpret_cast<volatile LONG*>(
        static_cast<std::uint8_t*>(value) + sizeof(void*));
    if (InterlockedDecrement(references) != 0) {
        return;
    }
    auto** vtable = *reinterpret_cast<void***>(value);
    using Destroy = void(__thiscall*)(void*, std::uint32_t);
    reinterpret_cast<Destroy>(vtable[0])(value, 1u);
}

void assign_client_reference(void** destination, void* value) noexcept {
    if (value != nullptr) {
        auto* references = reinterpret_cast<volatile LONG*>(
            static_cast<std::uint8_t*>(value) + sizeof(void*));
        InterlockedIncrement(references);
    }
    void* previous = *destination;
    *destination = value;
    release_client_reference(previous);
}

void mark_renderee_dirty(void* renderee) noexcept {
    auto* bytes = static_cast<std::uint8_t*>(renderee);
    bytes[kRendereeDirtyOffset] = 1u;
    void* parent = *reinterpret_cast<void**>(bytes + kRendereeParentOffset);
    std::size_t visited = 0;
    while (parent != nullptr && visited++ < kMaximumSceneNodes) {
        auto* parent_bytes = static_cast<std::uint8_t*>(parent);
        if (parent_bytes[kRendereeAncestorDirtyOffset] == 1u) {
            break;
        }
        parent_bytes[kRendereeAncestorDirtyOffset] = 1u;
        parent = *reinterpret_cast<void**>(parent_bytes + kRendereeParentOffset);
    }
}

bool configure_billboard(void* billboard) noexcept {
    if (billboard == nullptr
        || g_renderer_property == nullptr
        || *g_renderer_property == nullptr
        || g_client_operator_new == nullptr
        || g_alpha_property_ctor == nullptr
        || g_set_renderee_depth == nullptr) {
        return false;
    }

    auto* bytes = static_cast<std::uint8_t*>(billboard);
    assign_client_reference(
        reinterpret_cast<void**>(bytes + kRendereeRendererPropertyOffset),
        *g_renderer_property);
    mark_renderee_dirty(billboard);

    void* alpha = g_client_operator_new(0x20u);
    if (alpha == nullptr) {
        return false;
    }
    alpha = g_alpha_property_ctor(alpha);
    if (alpha == nullptr) {
        return false;
    }
    auto* alpha_bytes = static_cast<std::uint8_t*>(alpha);
    alpha_bytes[0x08u] = 1u;
    *reinterpret_cast<std::uint32_t*>(alpha_bytes + 0x0Cu) = 5u;
    *reinterpret_cast<std::uint32_t*>(alpha_bytes + 0x10u) = 6u;
    assign_client_reference(
        reinterpret_cast<void**>(bytes + kRendereeAlphaPropertyOffset),
        alpha);
    mark_renderee_dirty(billboard);

    return g_set_renderee_depth(billboard, kAuraRenderDepthBits);
}

bool configure_charger_billboards(void* root) noexcept {
    if (root == nullptr || g_find_first_billboard == nullptr) {
        return false;
    }
    std::array<void*, kMaximumSceneNodes> pending{};
    std::size_t pending_count = 1;
    std::size_t configured = 0;
    pending[0] = root;

    while (pending_count != 0) {
        void* node = pending[--pending_count];
        if (g_find_first_billboard(node) == node) {
            if (!configure_billboard(node)) {
                return false;
            }
            ++configured;
            continue;
        }

        auto* bytes = static_cast<std::uint8_t*>(node);
        auto** begin = *reinterpret_cast<void***>(bytes + kElementChildrenBeginOffset);
        auto** end = *reinterpret_cast<void***>(bytes + kElementChildrenEndOffset);
        if (begin == nullptr || end == nullptr || end < begin) {
            continue;
        }
        const std::size_t child_count = static_cast<std::size_t>(end - begin);
        if (child_count > kMaximumSceneNodes - pending_count) {
            return false;
        }
        while (begin != end) {
            void* child = *begin++;
            if (child != nullptr) {
                pending[pending_count++] = child;
            }
        }
    }
    return configured != 0;
}

bool assign_charger_resource(void* effect) noexcept {
    if (effect == nullptr
        || g_resource_catalog == nullptr
        || g_resource_catalog_lookup == nullptr
        || g_resource_context_resolve == nullptr
        || g_resource_path_compose == nullptr
        || g_wide_string_ctor == nullptr
        || g_wide_string_dtor == nullptr
        || g_shared_value_dtor == nullptr
        || g_resource_assign == nullptr) {
        return false;
    }

    std::uint32_t group_string = 0;
    std::uint32_t resource_string = 0;
    std::uint32_t composed_value = 0;
    const void* descriptor = nullptr;
    bool group_constructed = false;
    bool resource_constructed = false;
    bool composed_constructed = false;

    __try {
        g_wide_string_ctor(&group_string, L"charger");
        group_constructed = true;
        g_wide_string_ctor(&resource_string, L"카트바디차저발동");
        resource_constructed = true;

        void* catalog = g_resource_catalog;
        void* lookup = g_resource_catalog_lookup;
        void* resolve = g_resource_context_resolve;
        void* compose = g_resource_path_compose;
        __asm {
            xor ecx, ecx
            push ecx
            mov edx, esp
            lea eax, group_string
            push eax
            push edx
            mov ecx, catalog
            call dword ptr [lookup]
            mov ecx, eax
            call dword ptr [resolve]
            lea ecx, resource_string
            push ecx
            lea edx, composed_value
            push edx
            call dword ptr [compose]
            add esp, 0Ch
            mov descriptor, eax
        }
        composed_constructed = true;
        if (descriptor == nullptr) {
            __leave;
        }
        auto* destination = static_cast<std::uint8_t*>(effect) + kAttachedResourceOffset;
        g_resource_assign(destination, descriptor);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        descriptor = nullptr;
    }

    if (composed_constructed) {
        __try {
            g_shared_value_dtor(&composed_value);
        } __except (EXCEPTION_EXECUTE_HANDLER) {
        }
    }
    if (resource_constructed) {
        __try {
            g_wide_string_dtor(&resource_string);
        } __except (EXCEPTION_EXECUTE_HANDLER) {
        }
    }
    if (group_constructed) {
        __try {
            g_wide_string_dtor(&group_string);
        } __except (EXCEPTION_EXECUTE_HANDLER) {
        }
    }
    return descriptor != nullptr;
}

}  // namespace

void p5136_xun_configure_charger_visual(std::uintptr_t image_base) noexcept {
    g_image_base = image_base;
    g_resource_catalog = reinterpret_cast<void*>(image_base + kResourceCatalogRva);
    g_resource_catalog_lookup = reinterpret_cast<void*>(image_base + kResourceCatalogLookupRva);
    g_resource_context_resolve = reinterpret_cast<void*>(image_base + kResourceContextResolveRva);
    g_resource_path_compose = reinterpret_cast<void*>(image_base + kResourcePathComposeRva);
    g_wide_string_ctor = reinterpret_cast<WideStringCtor>(image_base + kWideStringCtorRva);
    g_wide_string_dtor = reinterpret_cast<WideStringDtor>(image_base + kWideStringDtorRva);
    g_shared_value_dtor = reinterpret_cast<SharedValueDtor>(image_base + kSharedValueDtorRva);
    g_resource_assign = reinterpret_cast<ResourceAssign>(image_base + kResourceAssignRva);
    g_crash_effect_ctor = reinterpret_cast<CrashEffectCtor>(image_base + kCrashEffectCtorRva);
    g_crash_effect_attach = reinterpret_cast<CrashEffectAttach>(image_base + kCrashEffectAttachRva);
    g_start_resource_children = reinterpret_cast<StartResourceChildren>(
        image_base + kStartResourceChildrenRva);
    g_stop_resource_children = reinterpret_cast<StopResourceChildren>(
        image_base + kStopResourceChildrenRva);
    g_find_first_billboard = reinterpret_cast<FindFirstBillboard>(
        image_base + kFindFirstBillboardRva);
    g_set_renderee_depth = reinterpret_cast<SetRendereeDepth>(
        image_base + kSetRendereeDepthRva);
    g_renderer_property = reinterpret_cast<void**>(image_base + kRendererPropertyRva);
    g_client_operator_new = reinterpret_cast<ClientOperatorNew>(
        image_base + kClientOperatorNewRva);
    g_alpha_property_ctor = reinterpret_cast<AlphaPropertyCtor>(
        image_base + kAlphaPropertyCtorRva);
}

void* p5136_xun_create_charger_visual(void* kart) noexcept {
    if (g_image_base == 0
        || kart == nullptr
        || g_crash_effect_ctor == nullptr
        || g_crash_effect_attach == nullptr) {
        return nullptr;
    }

    void* effect = HeapAlloc(GetProcessHeap(), HEAP_ZERO_MEMORY, kCrashEffectSize);
    if (effect == nullptr) {
        return nullptr;
    }
    bool constructed = false;
    __try {
        effect = g_crash_effect_ctor(effect);
        constructed = effect != nullptr;
        if (!constructed || !assign_charger_resource(effect)) {
            __leave;
        }
        void* resource = attached_resource(effect);
        if (resource == nullptr || !configure_charger_billboards(resource)) {
            __leave;
        }
        g_crash_effect_attach(effect, kart);
        auto** vtable = *reinterpret_cast<void***>(effect);
        reinterpret_cast<SetEffectActive>(vtable[12])(effect, 0, 0);
        if (g_stop_resource_children == nullptr) {
            __leave;
        }
        g_stop_resource_children(resource);
        return effect;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
    }

    // A constructed client object owns reference-counted renderer state. Its
    // destructor uses the client's allocator, while this compatibility object
    // intentionally lives for the process lifetime. Do not partially destroy
    // it after an exception; leaking this one failed probe is safer than
    // crossing allocator boundaries.
    if (!constructed) {
        HeapFree(GetProcessHeap(), 0, effect);
    }
    return nullptr;
}

bool p5136_xun_set_charger_visual_active(
    void* effect,
    bool active,
    std::uint32_t now_ms) noexcept {
    if (effect == nullptr) {
        return false;
    }
    __try {
        void* resource = attached_resource(effect);
        if (resource == nullptr
            || g_start_resource_children == nullptr
            || g_stop_resource_children == nullptr) {
            return false;
        }
        auto** vtable = *reinterpret_cast<void***>(effect);
        if (active) {
            // This mirrors modern ReChargerEffect::start: enable the root
            // scene, then start every child emitter at the current race time.
            reinterpret_cast<SetEffectActive>(vtable[12])(effect, 1u, 0u);
            g_start_resource_children(resource, now_ms, 0u);
        } else {
            // Stop child emitters before disabling the root, matching the
            // modern ReChargerEffect shutdown order.
            g_stop_resource_children(resource);
            reinterpret_cast<SetEffectActive>(vtable[12])(effect, 0u, 0u);
        }
        return true;
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        return false;
    }
}

void p5136_xun_tick_charger_visual(void* effect, std::uint32_t now_ms) noexcept {
    if (effect == nullptr) {
        return;
    }
    __try {
        auto** vtable = *reinterpret_cast<void***>(effect);
        reinterpret_cast<TickEffect>(vtable[16])(effect, now_ms);
    } __except (EXCEPTION_EXECUTE_HANDLER) {
    }
}
