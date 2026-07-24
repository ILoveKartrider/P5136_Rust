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

#[cfg(test)]
mod tests {
    use super::packet_hash;

    #[test]
    fn matches_p5136_packet_name_goldens() {
        assert_eq!(packet_hash("PcFirstMessage"), 0x282b_0580);
        assert_eq!(packet_hash("PqCnAuthenLogin"), 0x2d22_05d0);
        assert_eq!(packet_hash("PqLogin"), 0x0a83_02ba);
        assert_eq!(packet_hash("PrLogin"), 0x0a89_02bb);
    }
}
