//! Private server-to-sidecar protocol for the optional XUN physics backport.
//!
//! This transport is deliberately separate from the P5136 game protocol.  A
//! stock client never sees these bytes; only the version-pinned sidecar opens
//! the auxiliary TCP connection.

pub const XUN_SIDECAR_PROTOCOL_VERSION: u16 = 2;
pub const XUN_SIDECAR_HANDSHAKE_MAGIC: [u8; 4] = *b"P5XC";
pub const XUN_SIDECAR_PROFILE_MAGIC: [u8; 4] = *b"P5XP";
pub const XUN_SIDECAR_CLIENT_EVENT_MAGIC: [u8; 4] = *b"P5XE";
pub const XUN_SIDECAR_HANDSHAKE_HEADER_LENGTH: usize = 8;
pub const XUN_SIDECAR_PROFILE_FRAME_LENGTH: usize = 52;
pub const XUN_SIDECAR_MAX_NICKNAME_BYTES: usize = 128;
pub const XUN_SIDECAR_CLIENT_EVENT_HEADER_LENGTH: usize = 12;
pub const XUN_SIDECAR_MAX_CLIENT_EVENT_LENGTH: usize = 1_024;
pub const XUN_SIDECAR_CLIENT_EVENT_RACE_RESET: u16 = 1;

pub const XUN_PROFILE_FLAG_SPEED_BOOST_GAUGE: u32 = 1 << 0;
pub const XUN_PROFILE_FLAG_REMAINING_CONSUMERS: u32 = 1 << 1;

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum XunProfileState {
    #[default]
    Disabled = 0,
    SupportedSpeed = 1,
    ItemMode = 2,
    UnsupportedSpecial = 3,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct XunProfileFrame {
    pub generation: u32,
    pub kart_id: u16,
    pub exceed_type: u8,
    pub state: XunProfileState,
    pub flags: u32,
    pub booster_use_count: u32,
    pub use_time_ms: u32,
    pub charge_boost_by_speed_multiplier: f32,
    pub drift_gauge_factor: f32,
    pub wall_gauge_added: f32,
    pub boost_gauge_added: f32,
    pub anti_collide_balance: f32,
    pub default_engine_type: u8,
    pub default_handle_type: u8,
    pub default_wheel_type: u8,
    pub default_booster_type: u8,
}

impl XunProfileFrame {
    #[must_use]
    pub const fn disabled(kart_id: u16) -> Self {
        Self {
            generation: 0,
            kart_id,
            exceed_type: 0,
            state: XunProfileState::Disabled,
            flags: 0,
            booster_use_count: 0,
            use_time_ms: 0,
            charge_boost_by_speed_multiplier: 1.0,
            drift_gauge_factor: 0.0,
            wall_gauge_added: 0.0,
            boost_gauge_added: 0.0,
            anti_collide_balance: 0.0,
            default_engine_type: 0,
            default_handle_type: 0,
            default_wheel_type: 0,
            default_booster_type: 0,
        }
    }

    #[must_use]
    pub fn encode(self) -> [u8; XUN_SIDECAR_PROFILE_FRAME_LENGTH] {
        let mut output = [0_u8; XUN_SIDECAR_PROFILE_FRAME_LENGTH];
        output[0..4].copy_from_slice(&XUN_SIDECAR_PROFILE_MAGIC);
        output[4..6].copy_from_slice(&XUN_SIDECAR_PROTOCOL_VERSION.to_le_bytes());
        output[6..8].copy_from_slice(
            &u16::try_from(XUN_SIDECAR_PROFILE_FRAME_LENGTH)
                .expect("the fixed XUN frame length fits u16")
                .to_le_bytes(),
        );
        output[8..12].copy_from_slice(&self.generation.to_le_bytes());
        output[12..14].copy_from_slice(&self.kart_id.to_le_bytes());
        output[14] = self.exceed_type;
        output[15] = self.state as u8;
        output[16..20].copy_from_slice(&self.flags.to_le_bytes());
        output[20..24].copy_from_slice(&self.booster_use_count.to_le_bytes());
        output[24..28].copy_from_slice(&self.use_time_ms.to_le_bytes());
        output[28..32].copy_from_slice(&self.charge_boost_by_speed_multiplier.to_le_bytes());
        output[32..36].copy_from_slice(&self.drift_gauge_factor.to_le_bytes());
        output[36..40].copy_from_slice(&self.wall_gauge_added.to_le_bytes());
        output[40..44].copy_from_slice(&self.boost_gauge_added.to_le_bytes());
        output[44..48].copy_from_slice(&self.anti_collide_balance.to_le_bytes());
        output[48] = self.default_engine_type;
        output[49] = self.default_handle_type;
        output[50] = self.default_wheel_type;
        output[51] = self.default_booster_type;
        output
    }
}

#[must_use]
pub fn encode_xun_sidecar_handshake(nickname: &str) -> Option<Vec<u8>> {
    let nickname = nickname.as_bytes();
    if nickname.is_empty() || nickname.len() > XUN_SIDECAR_MAX_NICKNAME_BYTES {
        return None;
    }
    let nickname_length = u16::try_from(nickname.len()).ok()?;
    let mut output = Vec::with_capacity(XUN_SIDECAR_HANDSHAKE_HEADER_LENGTH + nickname.len());
    output.extend_from_slice(&XUN_SIDECAR_HANDSHAKE_MAGIC);
    output.extend_from_slice(&XUN_SIDECAR_PROTOCOL_VERSION.to_le_bytes());
    output.extend_from_slice(&nickname_length.to_le_bytes());
    output.extend_from_slice(nickname);
    Some(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_frame_has_stable_little_endian_layout() {
        let frame = XunProfileFrame {
            generation: 0x1122_3344,
            kart_id: 0x5566,
            exceed_type: 3,
            state: XunProfileState::SupportedSpeed,
            flags: XUN_PROFILE_FLAG_SPEED_BOOST_GAUGE | XUN_PROFILE_FLAG_REMAINING_CONSUMERS,
            booster_use_count: 5,
            use_time_ms: 3_750,
            charge_boost_by_speed_multiplier: 350.0,
            drift_gauge_factor: 2.0,
            wall_gauge_added: 0.09,
            boost_gauge_added: 0.03,
            anti_collide_balance: 0.8,
            default_engine_type: 21,
            default_handle_type: 22,
            default_wheel_type: 23,
            default_booster_type: 24,
        }
        .encode();
        assert_eq!(&frame[0..4], b"P5XP");
        assert_eq!(u16::from_le_bytes(frame[4..6].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(frame[6..8].try_into().unwrap()), 52);
        assert_eq!(
            u32::from_le_bytes(frame[8..12].try_into().unwrap()),
            0x1122_3344
        );
        assert_eq!(
            u16::from_le_bytes(frame[12..14].try_into().unwrap()),
            0x5566
        );
        assert_eq!(frame[14], 3);
        assert_eq!(frame[15], 1);
        assert_eq!(u32::from_le_bytes(frame[20..24].try_into().unwrap()), 5);
        assert_eq!(u32::from_le_bytes(frame[24..28].try_into().unwrap()), 3_750);
        assert_eq!(&frame[48..52], &[21, 22, 23, 24]);
    }

    #[test]
    fn handshake_bounds_nickname_bytes() {
        let encoded = encode_xun_sidecar_handshake("다오").unwrap();
        assert_eq!(&encoded[0..4], b"P5XC");
        assert_eq!(u16::from_le_bytes(encoded[6..8].try_into().unwrap()), 6);
        assert!(encode_xun_sidecar_handshake("").is_none());
        assert!(encode_xun_sidecar_handshake(&"x".repeat(129)).is_none());
    }
}
