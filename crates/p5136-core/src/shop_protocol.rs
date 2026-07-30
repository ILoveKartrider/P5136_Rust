//! Fail-closed P5136 shop-buy packet primitives.
//!
//! Stock-executable producer evidence establishes two exact request bodies:
//! the normal request carries two `i32` values and one `u8`, while the item
//! preset request adds one trailing `u16`. The field widths and order are
//! enforced here without assigning unproven value semantics or inventing
//! allowed ranges for `unknown`, `mode`, or the preset/slot value.

use thiserror::Error;

use crate::packet::{PacketError, PacketReader, PacketWriter};

pub const NORMAL_SHOP_BUY_ITEM_REQUEST_NAME: &str = "SpReqNormalShopBuyItemPacket";
pub const ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_NAME: &str = "SpReqItemPresetShopBuyItemPacket";
pub const BUY_ITEM_REPLY_NAME: &str = "SpRepBuyItemPacket";

pub const NORMAL_SHOP_BUY_ITEM_REQUEST_HASH: u32 = 0x9E70_0B05;
pub const ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH: u32 = 0xCE5F_0C9E;
pub const BUY_ITEM_REPLY_HASH: u32 = 0x415B_0701;

pub const NORMAL_SHOP_BUY_ITEM_BODY_LENGTH: usize = 9;
pub const ITEM_PRESET_SHOP_BUY_ITEM_BODY_LENGTH: usize = 11;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShopBuyRequest {
    Normal,
    ItemPreset,
}

impl ShopBuyRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::Normal => NORMAL_SHOP_BUY_ITEM_REQUEST_NAME,
            Self::ItemPreset => ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_NAME,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShopBuyVariant {
    Normal,
    ItemPreset { preset_or_slot: u16 },
}

/// A fully validated, exactly consumed P5136 shop-buy request.
///
/// The private tagged variant makes it impossible for callers to construct a
/// normal request with a preset/slot value, or an item-preset request without
/// one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParsedShopBuyRequest {
    stock_id: i32,
    unknown: i32,
    mode: u8,
    variant: ShopBuyVariant,
}

impl ParsedShopBuyRequest {
    #[must_use]
    pub const fn kind(&self) -> ShopBuyRequest {
        match self.variant {
            ShopBuyVariant::Normal => ShopBuyRequest::Normal,
            ShopBuyVariant::ItemPreset { .. } => ShopBuyRequest::ItemPreset,
        }
    }

    #[must_use]
    pub const fn stock_id(&self) -> i32 {
        self.stock_id
    }

    #[must_use]
    pub const fn unknown(&self) -> i32 {
        self.unknown
    }

    #[must_use]
    pub const fn mode(&self) -> u8 {
        self.mode
    }

    #[must_use]
    pub const fn preset_or_slot(&self) -> Option<u16> {
        match self.variant {
            ShopBuyVariant::Normal => None,
            ShopBuyVariant::ItemPreset { preset_or_slot } => Some(preset_or_slot),
        }
    }
}

#[derive(Debug, Error)]
pub enum ShopProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported P5136 shop-buy packet hash 0x{actual:08X}")]
    UnsupportedPacketHash { actual: u32 },

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub const fn classify_shop_buy_request(hash: u32) -> Option<ShopBuyRequest> {
    match hash {
        NORMAL_SHOP_BUY_ITEM_REQUEST_HASH => Some(ShopBuyRequest::Normal),
        ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH => Some(ShopBuyRequest::ItemPreset),
        _ => None,
    }
}

/// Parses an exact stock-client shop-buy request without allocating.
///
/// All scalar values are preserved as produced. Their business meaning and
/// allowed ranges are not yet evidenced, so this wire parser applies no policy
/// validation beyond the packet hash and exact shape.
pub fn parse_shop_buy_request(packet: &[u8]) -> Result<ParsedShopBuyRequest, ShopProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    let kind = classify_shop_buy_request(hash)
        .ok_or(ShopProtocolError::UnsupportedPacketHash { actual: hash })?;
    let stock_id = reader.read_i32()?;
    let unknown = reader.read_i32()?;
    let mode = reader.read_u8()?;
    let variant = match kind {
        ShopBuyRequest::Normal => ShopBuyVariant::Normal,
        ShopBuyRequest::ItemPreset => ShopBuyVariant::ItemPreset {
            preset_or_slot: reader.read_u16()?,
        },
    };
    ensure_exhausted(&reader, kind.request_name())?;
    Ok(ParsedShopBuyRequest {
        stock_id,
        unknown,
        mode,
        variant,
    })
}

/// Serializes the common P5136 fail-closed shop-buy response.
///
/// Both recognized request aliases receive the same failure packet, so the
/// request kind is intentionally not accepted here.
#[must_use]
pub fn serialize_shop_buy_failure() -> Vec<u8> {
    let mut packet = PacketWriter::named(BUY_ITEM_REPLY_NAME);
    packet.write_u8(1);
    packet.write_bytes(&[0; 24]);
    packet.into_inner()
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), ShopProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(ShopProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BUY_ITEM_REPLY_HASH, BUY_ITEM_REPLY_NAME, ITEM_PRESET_SHOP_BUY_ITEM_BODY_LENGTH,
        ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH, ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_NAME,
        NORMAL_SHOP_BUY_ITEM_BODY_LENGTH, NORMAL_SHOP_BUY_ITEM_REQUEST_HASH,
        NORMAL_SHOP_BUY_ITEM_REQUEST_NAME, ShopBuyRequest, ShopProtocolError,
        classify_shop_buy_request, parse_shop_buy_request, serialize_shop_buy_failure,
    };
    use crate::{adler32, packet::PacketError};

    const NORMAL_FIXTURE: [u8; 13] = [
        0x05, 0x0B, 0x70, 0x9E, // request hash
        0xC7, 0xCF, 0xFF, 0xFF, // stock_id = -12345
        0x00, 0x00, 0x00, 0x80, // unknown = i32::MIN
        0xFF, // mode
    ];
    const ITEM_PRESET_FIXTURE: [u8; 15] = [
        0x9E, 0x0C, 0x5F, 0xCE, // request hash
        0x78, 0x56, 0x34, 0x12, // stock_id = 0x12345678
        0xFE, 0xFF, 0xFF, 0xFF, // unknown = -2
        0x03, // mode
        0xEF, 0xBE, // preset_or_slot = 0xBEEF
    ];

    #[test]
    fn packet_names_match_the_exact_p5136_hashes() {
        assert_eq!(
            adler32::packet_hash(NORMAL_SHOP_BUY_ITEM_REQUEST_NAME),
            NORMAL_SHOP_BUY_ITEM_REQUEST_HASH
        );
        assert_eq!(
            adler32::packet_hash(ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_NAME),
            ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH
        );
        assert_eq!(
            adler32::packet_hash(BUY_ITEM_REPLY_NAME),
            BUY_ITEM_REPLY_HASH
        );
    }

    #[test]
    fn classifier_distinguishes_both_request_aliases() {
        fn assert_copy_and_eq<T: Copy + Eq>() {}
        assert_copy_and_eq::<ShopBuyRequest>();

        assert_eq!(
            classify_shop_buy_request(NORMAL_SHOP_BUY_ITEM_REQUEST_HASH),
            Some(ShopBuyRequest::Normal)
        );
        assert_eq!(
            classify_shop_buy_request(ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH),
            Some(ShopBuyRequest::ItemPreset)
        );
        assert_eq!(
            ShopBuyRequest::Normal.request_name(),
            NORMAL_SHOP_BUY_ITEM_REQUEST_NAME
        );
        assert_eq!(
            ShopBuyRequest::ItemPreset.request_name(),
            ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_NAME
        );
    }

    #[test]
    fn classifier_rejects_unknown_hashes() {
        assert_eq!(classify_shop_buy_request(0), None);
        assert_eq!(classify_shop_buy_request(BUY_ITEM_REPLY_HASH), None);
        assert_eq!(classify_shop_buy_request(0xDEAD_BEEF), None);
    }

    #[test]
    fn both_exact_request_fixtures_parse_with_unrestricted_scalar_values() {
        let normal = parse_shop_buy_request(&NORMAL_FIXTURE).unwrap();
        assert_eq!(normal.kind(), ShopBuyRequest::Normal);
        assert_eq!(normal.stock_id(), -12_345);
        assert_eq!(normal.unknown(), i32::MIN);
        assert_eq!(normal.mode(), u8::MAX);
        assert_eq!(normal.preset_or_slot(), None);

        let item_preset = parse_shop_buy_request(&ITEM_PRESET_FIXTURE).unwrap();
        assert_eq!(item_preset.kind(), ShopBuyRequest::ItemPreset);
        assert_eq!(item_preset.stock_id(), 0x1234_5678);
        assert_eq!(item_preset.unknown(), -2);
        assert_eq!(item_preset.mode(), 3);
        assert_eq!(item_preset.preset_or_slot(), Some(0xBEEF));

        assert_eq!(NORMAL_FIXTURE.len() - 4, NORMAL_SHOP_BUY_ITEM_BODY_LENGTH);
        assert_eq!(
            ITEM_PRESET_FIXTURE.len() - 4,
            ITEM_PRESET_SHOP_BUY_ITEM_BODY_LENGTH
        );
    }

    #[test]
    fn every_truncated_prefix_of_each_exact_fixture_is_rejected() {
        for fixture in [&NORMAL_FIXTURE[..], &ITEM_PRESET_FIXTURE[..]] {
            for length in 0..fixture.len() {
                assert!(
                    matches!(
                        parse_shop_buy_request(&fixture[..length]),
                        Err(ShopProtocolError::Packet(PacketError::Truncated { .. }))
                    ),
                    "prefix {length} of {} unexpectedly parsed",
                    fixture.len()
                );
            }
        }
    }

    #[test]
    fn wrong_and_unknown_packet_hashes_are_rejected_before_body_parsing() {
        for hash in [BUY_ITEM_REPLY_HASH, 0xDEAD_BEEF] {
            let mut packet = NORMAL_FIXTURE;
            packet[..4].copy_from_slice(&hash.to_le_bytes());
            assert!(matches!(
                parse_shop_buy_request(&packet),
                Err(ShopProtocolError::UnsupportedPacketHash { actual }) if actual == hash
            ));
        }
    }

    #[test]
    fn cross_kind_length_drift_and_all_trailing_bytes_are_rejected() {
        let mut normal_shape_with_preset_hash = NORMAL_FIXTURE;
        normal_shape_with_preset_hash[..4]
            .copy_from_slice(&ITEM_PRESET_SHOP_BUY_ITEM_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_shop_buy_request(&normal_shape_with_preset_hash),
            Err(ShopProtocolError::Packet(PacketError::Truncated { .. }))
        ));

        let mut preset_shape_with_normal_hash = ITEM_PRESET_FIXTURE;
        preset_shape_with_normal_hash[..4]
            .copy_from_slice(&NORMAL_SHOP_BUY_ITEM_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_shop_buy_request(&preset_shape_with_normal_hash),
            Err(ShopProtocolError::TrailingBytes { count: 2, .. })
        ));

        for mut packet in [NORMAL_FIXTURE.to_vec(), ITEM_PRESET_FIXTURE.to_vec()] {
            packet.push(0xA5);
            assert!(matches!(
                parse_shop_buy_request(&packet),
                Err(ShopProtocolError::TrailingBytes { count: 1, .. })
            ));
        }
    }

    #[test]
    fn failure_response_matches_the_exact_29_byte_layout() {
        let response = serialize_shop_buy_failure();
        let mut expected = [0_u8; 29];
        expected[..5].copy_from_slice(&[0x01, 0x07, 0x5B, 0x41, 0x01]);

        assert_eq!(response, expected);
        assert_eq!(response.len(), 29);
    }
}
