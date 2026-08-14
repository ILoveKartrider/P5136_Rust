cmake_minimum_required(VERSION 3.25)

project(p5136_xun_sidecar VERSION 1.0.0 LANGUAGES CXX)

if(NOT WIN32 OR NOT MSVC)
    message(FATAL_ERROR "p5136-xun-sidecar requires 32-bit MSVC")
endif()
if(NOT CMAKE_SIZEOF_VOID_P EQUAL 4)
    message(FATAL_ERROR "P5136 is a 32-bit client; configure CMake with -A Win32")
endif()

set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_CXX_EXTENSIONS OFF)
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")

add_library(p5136_xun_sidecar SHARED
    src/charger_visual.cpp
    src/charger_visual.hpp
    src/dinput8_proxy.cpp
    src/sidecar.cpp
    src/sidecar.hpp
    src/lifecycle_hooks.cpp
    src/lifecycle_hooks.hpp
    src/xun_physics_runtime.cpp
    src/xun_physics_runtime.hpp
    src/xun_physics_state.cpp
    src/xun_physics_state.hpp
    src/xun_profile_client.cpp
    src/xun_profile_client.hpp
    include/p5136_xun_api.h
    dinput8.def
)
target_include_directories(p5136_xun_sidecar PRIVATE include src)
target_compile_definitions(p5136_xun_sidecar PRIVATE
    WIN32_LEAN_AND_MEAN
    NOMINMAX
    UNICODE
    _UNICODE
    _WIN32_WINNT=0x0601
    DIRECTINPUT_VERSION=0x0800
)
target_compile_options(p5136_xun_sidecar PRIVATE
    /W4 /WX /permissive- /sdl /GS /Gy /Gw /utf-8
)
target_link_options(p5136_xun_sidecar PRIVATE /DYNAMICBASE /NXCOMPAT /OPT:REF /OPT:ICF /IGNORE:4104)
target_link_libraries(p5136_xun_sidecar PRIVATE advapi32 ws2_32)
set_target_properties(p5136_xun_sidecar PROPERTIES
    OUTPUT_NAME dinput8
    PREFIX ""
)

add_library(p5136_xun_attachable SHARED
    src/charger_visual.cpp
    src/charger_visual.hpp
    src/attach_dll.cpp
    src/sidecar.cpp
    src/sidecar.hpp
    src/lifecycle_hooks.cpp
    src/lifecycle_hooks.hpp
    src/xun_physics_runtime.cpp
    src/xun_physics_runtime.hpp
    src/xun_physics_state.cpp
    src/xun_physics_state.hpp
    src/xun_profile_client.cpp
    src/xun_profile_client.hpp
    include/p5136_xun_api.h
    p5136-xun.def
)
target_include_directories(p5136_xun_attachable PRIVATE include src)
target_compile_definitions(p5136_xun_attachable PRIVATE
    WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE _WIN32_WINNT=0x0601
)
target_compile_options(p5136_xun_attachable PRIVATE
    /W4 /WX /permissive- /sdl /GS /Gy /Gw /utf-8
)
target_link_options(p5136_xun_attachable PRIVATE /DYNAMICBASE /NXCOMPAT /OPT:REF /OPT:ICF /IGNORE:4104)
target_link_libraries(p5136_xun_attachable PRIVATE advapi32 ws2_32)
set_target_properties(p5136_xun_attachable PROPERTIES
    OUTPUT_NAME p5136-xun
    PREFIX ""
)

add_executable(
    p5136_xun_attach
    src/charger_visual.cpp
    src/charger_visual.hpp
    src/attach_tool.cpp
    src/sidecar.cpp
    src/sidecar.hpp
    src/lifecycle_hooks.cpp
    src/lifecycle_hooks.hpp
    src/xun_physics_runtime.cpp
    src/xun_physics_runtime.hpp
    src/xun_physics_state.cpp
    src/xun_physics_state.hpp
    src/xun_profile_client.cpp
    src/xun_profile_client.hpp
    include/p5136_xun_api.h
)
target_include_directories(p5136_xun_attach PRIVATE include src)
target_compile_definitions(p5136_xun_attach PRIVATE
    WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE _WIN32_WINNT=0x0601
)
target_compile_options(p5136_xun_attach PRIVATE /W4 /WX /permissive- /sdl /GS /utf-8)
target_link_options(p5136_xun_attach PRIVATE /DYNAMICBASE /NXCOMPAT /OPT:REF /OPT:ICF)
target_link_libraries(p5136_xun_attach PRIVATE advapi32 ws2_32)
set_target_properties(p5136_xun_attach PROPERTIES OUTPUT_NAME p5136-xun-attach)

add_executable(p5136_xun_smoke tests/loader_smoke.cpp include/p5136_xun_api.h)
target_include_directories(p5136_xun_smoke PRIVATE include)
target_compile_definitions(p5136_xun_smoke PRIVATE
    WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE _WIN32_WINNT=0x0601 DIRECTINPUT_VERSION=0x0800
)
target_compile_options(p5136_xun_smoke PRIVATE /W4 /WX /permissive- /sdl /GS /utf-8)
target_link_libraries(p5136_xun_smoke PRIVATE dxguid)

add_executable(p5136_xun_attach_smoke tests/attach_smoke.cpp include/p5136_xun_api.h)
target_include_directories(p5136_xun_attach_smoke PRIVATE include)
target_compile_definitions(p5136_xun_attach_smoke PRIVATE
    WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE _WIN32_WINNT=0x0601
)
target_compile_options(p5136_xun_attach_smoke PRIVATE /W4 /WX /permissive- /sdl /GS /utf-8)

add_executable(p5136_xun_probe_site_smoke tests/probe_site_smoke.cpp)
target_compile_definitions(p5136_xun_probe_site_smoke PRIVATE
    WIN32_LEAN_AND_MEAN NOMINMAX UNICODE _UNICODE _WIN32_WINNT=0x0601
)
target_compile_options(p5136_xun_probe_site_smoke PRIVATE /W4 /WX /permissive- /sdl /GS /utf-8)

add_executable(
    p5136_xun_physics_state_smoke
    tests/physics_state_smoke.cpp
    src/xun_physics_state.cpp
    src/xun_physics_state.hpp
)
target_include_directories(p5136_xun_physics_state_smoke PRIVATE src)
target_compile_options(p5136_xun_physics_state_smoke PRIVATE /W4 /WX /permissive- /sdl /GS /utf-8)

enable_testing()
set(P5136_TEST_EXECUTABLE "" CACHE FILEPATH "Exact unpacked P5136 executable for sidecar verification")
if(P5136_TEST_EXECUTABLE)
    add_test(
        NAME p5136_xun_loader_smoke
        COMMAND p5136_xun_smoke $<TARGET_FILE:p5136_xun_sidecar> "${P5136_TEST_EXECUTABLE}"
    )
    add_test(
        NAME p5136_xun_attach_smoke
        COMMAND p5136_xun_attach_smoke $<TARGET_FILE:p5136_xun_attachable> "${P5136_TEST_EXECUTABLE}"
    )
    add_test(
        NAME p5136_xun_probe_site_smoke
        COMMAND p5136_xun_probe_site_smoke "${P5136_TEST_EXECUTABLE}"
    )
endif()
add_test(NAME p5136_xun_physics_state_smoke COMMAND p5136_xun_physics_state_smoke)

install(
    TARGETS p5136_xun_sidecar p5136_xun_attachable p5136_xun_attach
    RUNTIME DESTINATION .
    LIBRARY DESTINATION .
)
install(FILES p5136-xun.example.ini README.md DESTINATION .)
