#include <windows.h>

#include <array>
#include <cstdint>
#include <cstdio>
#include <cstring>
#include <vector>

namespace {

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
constexpr std::array<BYTE, 5> kExpectedSpeedBoostGaugeAddCall1 = {
    0xE8, 0xDE, 0xE6, 0xF2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedSpeedBoostGaugeAddCall2 = {
    0xE8, 0x8F, 0xE6, 0xF2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedSpeedBoostGaugeAddCall3 = {
    0xE8, 0x50, 0xE6, 0xF2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedDriftGaugeCall = {
    0xE8, 0x9B, 0x46, 0xC3, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedCollisionResponseCall = {
    0xE8, 0x73, 0x69, 0x00, 0x00,
};
constexpr std::array<BYTE, 5> kExpectedAntiCollideCall1 = {
    0xE8, 0x8A, 0xB4, 0xC2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedAntiCollideCall2 = {
    0xE8, 0x15, 0xB4, 0xC2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedWallGaugeCall = {
    0xE8, 0x92, 0x97, 0xC2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedBoostGaugeCall = {
    0xE8, 0x38, 0x94, 0xC2, 0xFF,
};
constexpr std::array<BYTE, 5> kExpectedEffectRegistrationCall = {
    0xE8, 0xA9, 0x41, 0x53, 0x00,
};

bool read_file(const wchar_t* path, std::vector<BYTE>* output) {
    FILE* file = nullptr;
    if (_wfopen_s(&file, path, L"rb") != 0 || file == nullptr) {
        return false;
    }
    if (_fseeki64(file, 0, SEEK_END) != 0) {
        fclose(file);
        return false;
    }
    const __int64 length = _ftelli64(file);
    if (length <= 0 || _fseeki64(file, 0, SEEK_SET) != 0) {
        fclose(file);
        return false;
    }
    output->resize(static_cast<std::size_t>(length));
    const bool success = fread(output->data(), 1, output->size(), file) == output->size();
    fclose(file);
    return success;
}

bool rva_to_offset(const std::vector<BYTE>& image, DWORD rva, std::size_t* output) {
    if (image.size() < sizeof(IMAGE_DOS_HEADER)) {
        return false;
    }
    const auto* dos = reinterpret_cast<const IMAGE_DOS_HEADER*>(image.data());
    if (dos->e_magic != IMAGE_DOS_SIGNATURE || dos->e_lfanew <= 0) {
        return false;
    }
    const std::size_t nt_offset = static_cast<std::size_t>(dos->e_lfanew);
    if (nt_offset > image.size() - sizeof(IMAGE_NT_HEADERS32)) {
        return false;
    }
    const auto* nt = reinterpret_cast<const IMAGE_NT_HEADERS32*>(image.data() + nt_offset);
    if (nt->Signature != IMAGE_NT_SIGNATURE
        || nt->OptionalHeader.Magic != IMAGE_NT_OPTIONAL_HDR32_MAGIC) {
        return false;
    }
    if (rva < nt->OptionalHeader.SizeOfHeaders) {
        *output = rva;
        return *output < image.size();
    }
    const auto* section = IMAGE_FIRST_SECTION(nt);
    for (WORD index = 0; index < nt->FileHeader.NumberOfSections; ++index) {
        const DWORD span = section[index].Misc.VirtualSize > section[index].SizeOfRawData
            ? section[index].Misc.VirtualSize
            : section[index].SizeOfRawData;
        if (rva >= section[index].VirtualAddress
            && rva - section[index].VirtualAddress < span) {
            *output = static_cast<std::size_t>(section[index].PointerToRawData)
                + (rva - section[index].VirtualAddress);
            return *output < image.size();
        }
    }
    return false;
}

template<std::size_t Size>
bool verify_site(const std::vector<BYTE>& image, DWORD rva, const std::array<BYTE, Size>& expected) {
    std::size_t offset = 0;
    return rva_to_offset(image, rva, &offset)
        && offset <= image.size() - expected.size()
        && std::memcmp(image.data() + offset, expected.data(), expected.size()) == 0;
}

}  // namespace

int wmain(int argc, wchar_t** argv) {
    if (argc != 2) {
        std::fwprintf(stderr, L"usage: p5136_xun_probe_site_smoke <P5136.exe>\n");
        return 2;
    }
    std::vector<BYTE> image;
    if (!read_file(argv[1], &image)) {
        std::fwprintf(stderr, L"could not read P5136 executable\n");
        return 3;
    }
    if (!verify_site(image, 0x002C2980u, kExpectedTachoPrologue)
        || !verify_site(image, 0x002CECF0u, kExpectedTachoPrologue)
        || !verify_site(image, 0x00284960u, kExpectedFactoryLookupPrologue)
        || !verify_site(image, 0x002C11E0u, kExpectedV1TachoUpdatePrologue)
        || !verify_site(image, 0x002F64A0u, kExpectedDisplayStatPrologue)
        || !verify_site(image, 0x0062D84Du, kExpectedAcceptedDriveEventSite)
        || !verify_site(image, 0x00633420u, kExpectedPhysicsTickPrologue)
        || !verify_site(image, 0x0063481Du, kExpectedSpeedBoostGaugeAddCall1)
        || !verify_site(image, 0x0063486Cu, kExpectedSpeedBoostGaugeAddCall2)
        || !verify_site(image, 0x006348ABu, kExpectedSpeedBoostGaugeAddCall3)
        || !verify_site(image, 0x006349B0u, kExpectedDriftGaugeCall)
        || !verify_site(image, 0x0063B058u, kExpectedCollisionResponseCall)
        || !verify_site(image, 0x0063DBC1u, kExpectedAntiCollideCall1)
        || !verify_site(image, 0x0063DC36u, kExpectedAntiCollideCall2)
        || !verify_site(image, 0x0063F8B9u, kExpectedWallGaugeCall)
        || !verify_site(image, 0x0063FC13u, kExpectedBoostGaugeCall)
        || !verify_site(image, 0x00631E52u, kExpectedEffectRegistrationCall)) {
        std::fwprintf(stderr, L"tachometer factory, display conversion, physics-state, XUN consumer, or charger-effect site mismatch\n");
        return 4;
    }
    std::wprintf(L"tachometer factory, display conversion, physics-state, all six XUN consumers, and charger-effect registration match exact instruction boundaries\n");
    return 0;
}
