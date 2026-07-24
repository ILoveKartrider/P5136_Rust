//! Packet-name hashing used by the client RTTI dispatcher.

const MOD_ADLER: u32 = 65_521;

/// Computes the same seeded Adler-32 variant as the legacy server.
///
/// Packet names use a seed of zero, unlike the conventional Adler-32 seed of
/// one.
#[must_use]
pub fn hash(bytes: &[u8], seed: u32) -> u32 {
    let mut a = seed & 0xffff;
    let mut b = seed >> 16;

    for &byte in bytes {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }

    a | (b << 16)
}

/// Computes the RTTI hash written at the start of every named packet.
#[must_use]
pub fn packet_hash(name: &str) -> u32 {
    hash(name.as_bytes(), 0)
}

/// Computes the zero-seeded Adler-32 of a .NET `Encoding.Unicode` string.
///
/// P5136 uses this UTF-16LE form for track identifiers and several RHO
/// resource names, while packet RTTI names use UTF-8/ASCII bytes.
#[must_use]
pub fn unicode_hash(value: &str) -> u32 {
    let mut a = 0;
    let mut b = 0;
    for byte in value.encode_utf16().flat_map(u16::to_le_bytes) {
        a = (a + u32::from(byte)) % MOD_ADLER;
        b = (b + a) % MOD_ADLER;
    }
    a | (b << 16)
}

#[cfg(test)]
mod tests {
    use super::{packet_hash, unicode_hash};

    #[test]
    fn matches_p5136_packet_name_goldens() {
        assert_eq!(packet_hash("PcFirstMessage"), 0x282b_0580);
        assert_eq!(packet_hash("PqCnAuthenLogin"), 0x2d22_05d0);
        assert_eq!(packet_hash("PrCnAuthenLogin"), 0x2d30_05d1);
        assert_eq!(packet_hash("PqLogin"), 0x0a83_02ba);
        assert_eq!(packet_hash("PrLogin"), 0x0a89_02bb);
        assert_eq!(packet_hash("AccountDataProfile"), 0x422b_0718);
        assert_eq!(packet_hash("PqChannelSwitch"), 0x2e09_05ec);
        assert_eq!(packet_hash("PrChannelSwitch"), 0x2e17_05ed);
        assert_eq!(packet_hash("PqChannelMovein"), 0x2dd6_05e8);
        assert_eq!(packet_hash("PrChannelMoveIn"), 0x2da4_05c9);
    }

    #[test]
    fn matches_dotnet_unicode_track_hash() {
        assert_eq!(unicode_hash("village_R01"), 0x34ca_03f6);
    }
}
