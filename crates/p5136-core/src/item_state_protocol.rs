//! Strict P5136 item-state request primitives.
//!
//! Stock-executable evidence establishes four request shapes used by the
//! remaining item-state surface. Delete and unlock requests carry the common
//! `IngameAuthData` prefix, but their observed producers leave it at the
//! canonical zero/empty value. Favorite updates are one-way batches capped at
//! 200 records by the stock producer. This module validates those exact
//! producer forms and exposes no delete/unlock success serializer: either
//! reply would authorize a state transition that the Rust server cannot yet
//! perform.

use thiserror::Error;

use crate::{
    frame::DEFAULT_MAX_PAYLOAD,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const DELETE_ITEM_REQUEST_NAME: &str = "LoRqDeleteItemPacket";
pub const DELETE_ITEM_REPLY_NAME: &str = "LoRpDeleteItemPacket";
pub const UNLOCK_ITEM_REQUEST_NAME: &str = "PqUnLockedItem";
pub const UNLOCK_ITEM_REPLY_NAME: &str = "PrUnLockedItem";
pub const FAVORITE_ITEM_GET_REQUEST_NAME: &str = "PqFavoriteItemGet";
pub const FAVORITE_ITEM_GET_REPLY_NAME: &str = "PrFavoriteItemGet";
pub const FAVORITE_ITEM_UPDATE_REQUEST_NAME: &str = "PqFavoriteItemUpdate";

pub const DELETE_ITEM_REQUEST_HASH: u32 = 0x4F4E_07B8;
pub const DELETE_ITEM_REPLY_HASH: u32 = 0x4F3D_07B7;
pub const UNLOCK_ITEM_REQUEST_HASH: u32 = 0x27C0_0565;
pub const UNLOCK_ITEM_REPLY_HASH: u32 = 0x27CD_0566;
pub const FAVORITE_ITEM_GET_REQUEST_HASH: u32 = 0x3BAD_06B0;
pub const FAVORITE_ITEM_GET_REPLY_HASH: u32 = 0x3BBD_06B1;
pub const FAVORITE_ITEM_UPDATE_REQUEST_HASH: u32 = 0x5278_07F3;

/// Exact stock producer cap for one favorite-item update batch.
pub const MAX_FAVORITE_ITEM_UPDATE_RECORDS: usize = 200;
const MAX_FAVORITE_ITEM_UPDATE_RECORDS_U32: u32 = 200;

pub const FAVORITE_ITEM_LIST_HEADER_LENGTH: usize = 8;
pub const FAVORITE_ITEM_LIST_RECORD_LENGTH: usize = 7;

/// Rust's default aggregate favorite-list bound, derived from the default
/// login-frame payload cap rather than the stock producer's per-batch cap.
///
/// Stock proves only that one update contains at most 200 records. Multiple
/// batches can grow the stable collection beyond 200, so persistence and reply
/// planning use this independent, operational bound.
pub const DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS: usize =
    (DEFAULT_MAX_PAYLOAD - FAVORITE_ITEM_LIST_HEADER_LENGTH) / FAVORITE_ITEM_LIST_RECORD_LENGTH;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemStateRequest {
    DeleteItem,
    UnlockItem,
    FavoriteItemGet,
    FavoriteItemUpdate,
}

impl ItemStateRequest {
    #[must_use]
    pub const fn request_name(self) -> &'static str {
        match self {
            Self::DeleteItem => DELETE_ITEM_REQUEST_NAME,
            Self::UnlockItem => UNLOCK_ITEM_REQUEST_NAME,
            Self::FavoriteItemGet => FAVORITE_ITEM_GET_REQUEST_NAME,
            Self::FavoriteItemUpdate => FAVORITE_ITEM_UPDATE_REQUEST_NAME,
        }
    }

    #[must_use]
    pub const fn reply_name(self) -> Option<&'static str> {
        match self {
            Self::DeleteItem => Some(DELETE_ITEM_REPLY_NAME),
            Self::UnlockItem => Some(UNLOCK_ITEM_REPLY_NAME),
            Self::FavoriteItemGet => Some(FAVORITE_ITEM_GET_REPLY_NAME),
            Self::FavoriteItemUpdate => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FavoriteItemKey {
    category: u16,
    item_id: u16,
    serial: u16,
}

impl FavoriteItemKey {
    #[must_use]
    pub const fn new(category: u16, item_id: u16, serial: u16) -> Self {
        Self {
            category,
            item_id,
            serial,
        }
    }

    #[must_use]
    pub const fn category(self) -> u16 {
        self.category
    }

    #[must_use]
    pub const fn item_id(self) -> u16 {
        self.item_id
    }

    #[must_use]
    pub const fn serial(self) -> u16 {
        self.serial
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FavoriteItemOperation {
    Add,
    Remove,
}

impl FavoriteItemOperation {
    #[must_use]
    pub const fn wire_value(self) -> u8 {
        match self {
            Self::Add => 1,
            Self::Remove => 2,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FavoriteItemChange {
    item: FavoriteItemKey,
    operation: FavoriteItemOperation,
}

impl FavoriteItemChange {
    #[must_use]
    pub const fn new(item: FavoriteItemKey, operation: FavoriteItemOperation) -> Self {
        Self { item, operation }
    }

    #[must_use]
    pub const fn item(self) -> FavoriteItemKey {
        self.item
    }

    #[must_use]
    pub const fn operation(self) -> FavoriteItemOperation {
        self.operation
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeleteItemFields {
    item: FavoriteItemKey,
    quantity_or_mode: u16,
}

impl DeleteItemFields {
    #[must_use]
    pub const fn item(self) -> FavoriteItemKey {
        self.item
    }

    #[must_use]
    pub const fn quantity_or_mode(self) -> u16 {
        self.quantity_or_mode
    }
}

#[derive(Clone, PartialEq, Eq)]
enum ParsedItemStateFields {
    Delete(DeleteItemFields),
    Unlock,
    FavoriteGet,
    FavoriteUpdate(Vec<FavoriteItemChange>),
}

/// A fully validated and exactly consumed stock item-state request.
///
/// Construction and fields remain private so only this module's parser can
/// mint the proof consumed by session dispatch.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedItemStateRequest {
    kind: ItemStateRequest,
    fields: ParsedItemStateFields,
}

impl ParsedItemStateRequest {
    #[must_use]
    pub const fn kind(&self) -> ItemStateRequest {
        self.kind
    }

    #[must_use]
    pub fn delete_fields(&self) -> Option<DeleteItemFields> {
        match &self.fields {
            ParsedItemStateFields::Delete(fields) => Some(*fields),
            ParsedItemStateFields::Unlock
            | ParsedItemStateFields::FavoriteGet
            | ParsedItemStateFields::FavoriteUpdate(_) => None,
        }
    }

    #[must_use]
    pub fn favorite_changes(&self) -> Option<&[FavoriteItemChange]> {
        match &self.fields {
            ParsedItemStateFields::FavoriteUpdate(changes) => Some(changes),
            ParsedItemStateFields::Delete(_)
            | ParsedItemStateFields::Unlock
            | ParsedItemStateFields::FavoriteGet => None,
        }
    }

    pub fn into_favorite_changes(self) -> Result<Vec<FavoriteItemChange>, ItemStateProtocolError> {
        match self.fields {
            ParsedItemStateFields::FavoriteUpdate(changes) => Ok(changes),
            ParsedItemStateFields::Delete(_)
            | ParsedItemStateFields::Unlock
            | ParsedItemStateFields::FavoriteGet => {
                Err(ItemStateProtocolError::ParsedKindMismatch {
                    expected: ItemStateRequest::FavoriteItemUpdate,
                    actual: self.kind,
                })
            }
        }
    }
}

#[derive(Debug, Error)]
pub enum ItemStateProtocolError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unsupported P5136 item-state packet hash 0x{actual:08X}")]
    UnsupportedPacketHash { actual: u32 },

    #[error("parsed item-state request kind is {actual:?}; expected {expected:?}")]
    ParsedKindMismatch {
        expected: ItemStateRequest,
        actual: ItemStateRequest,
    },

    #[error("{name} stock producer auth scalar must be zero, received {actual}")]
    NonZeroProducerAuthScalar { name: &'static str, actual: u32 },

    #[error("{name} stock producer credential list must be empty, received count {count}")]
    NonEmptyProducerCredentials { name: &'static str, count: u32 },

    #[error("PqUnLockedItem stock producer terminal byte must be zero, received {actual}")]
    NonZeroUnlockProducerByte { actual: u8 },

    #[error("PqFavoriteItemUpdate producer scope must be one, received {actual}")]
    InvalidFavoriteUpdateScope { actual: u8 },

    #[error("PqFavoriteItemUpdate has {count} records; stock producer maximum is {maximum}")]
    FavoriteUpdateCountLimitExceeded { count: u32, maximum: usize },

    #[error("PqFavoriteItemUpdate record count {count} cannot fit this platform")]
    FavoriteUpdateCountOutOfRange { count: u32 },

    #[error(
        "PqFavoriteItemUpdate record {index} has unsupported operation {actual}; expected 1 or 2"
    )]
    InvalidFavoriteOperation { index: usize, actual: u8 },

    #[error(
        "favorite-item reply has {count} records; payload cap {maximum_payload} permits {maximum_records}"
    )]
    FavoriteListPayloadLimitExceeded {
        count: usize,
        maximum_records: usize,
        maximum_payload: usize,
    },

    #[error(
        "favorite-item reply header requires {minimum_payload} bytes; payload cap is {maximum_payload}"
    )]
    FavoriteListPayloadTooSmall {
        minimum_payload: usize,
        maximum_payload: usize,
    },

    #[error("packet {name} has {count} unexpected trailing bytes")]
    TrailingBytes { name: &'static str, count: usize },
}

#[must_use]
pub const fn classify_item_state_request(hash: u32) -> Option<ItemStateRequest> {
    match hash {
        DELETE_ITEM_REQUEST_HASH => Some(ItemStateRequest::DeleteItem),
        UNLOCK_ITEM_REQUEST_HASH => Some(ItemStateRequest::UnlockItem),
        FAVORITE_ITEM_GET_REQUEST_HASH => Some(ItemStateRequest::FavoriteItemGet),
        FAVORITE_ITEM_UPDATE_REQUEST_HASH => Some(ItemStateRequest::FavoriteItemUpdate),
        _ => None,
    }
}

/// Parses only the exact item-state forms emitted by the inspected stock
/// producers.
///
/// Delete and unlock reject nonempty authentication data before reading or
/// allocating any credential string. Favorite update bounds its raw `u32`
/// count before allocating the record vector and requires complete
/// consumption for every request kind.
pub fn parse_item_state_request(
    packet: &[u8],
) -> Result<ParsedItemStateRequest, ItemStateProtocolError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;
    let kind = classify_item_state_request(hash)
        .ok_or(ItemStateProtocolError::UnsupportedPacketHash { actual: hash })?;
    let fields = match kind {
        ItemStateRequest::DeleteItem => {
            parse_empty_producer_auth(&mut reader, kind.request_name())?;
            ParsedItemStateFields::Delete(DeleteItemFields {
                item: FavoriteItemKey::new(
                    reader.read_u16()?,
                    reader.read_u16()?,
                    reader.read_u16()?,
                ),
                quantity_or_mode: reader.read_u16()?,
            })
        }
        ItemStateRequest::UnlockItem => {
            parse_empty_producer_auth(&mut reader, kind.request_name())?;
            let terminal = reader.read_u8()?;
            if terminal != 0 {
                return Err(ItemStateProtocolError::NonZeroUnlockProducerByte { actual: terminal });
            }
            ParsedItemStateFields::Unlock
        }
        ItemStateRequest::FavoriteItemGet => ParsedItemStateFields::FavoriteGet,
        ItemStateRequest::FavoriteItemUpdate => {
            let scope = reader.read_u8()?;
            if scope != 1 {
                return Err(ItemStateProtocolError::InvalidFavoriteUpdateScope { actual: scope });
            }
            let wire_count = reader.read_u32()?;
            if wire_count > MAX_FAVORITE_ITEM_UPDATE_RECORDS_U32 {
                return Err(ItemStateProtocolError::FavoriteUpdateCountLimitExceeded {
                    count: wire_count,
                    maximum: MAX_FAVORITE_ITEM_UPDATE_RECORDS,
                });
            }
            let count = usize::try_from(wire_count).map_err(|_| {
                ItemStateProtocolError::FavoriteUpdateCountOutOfRange { count: wire_count }
            })?;
            let mut changes = Vec::with_capacity(count);
            for index in 0..count {
                let item = FavoriteItemKey::new(
                    reader.read_u16()?,
                    reader.read_u16()?,
                    reader.read_u16()?,
                );
                let actual = reader.read_u8()?;
                let operation = match actual {
                    1 => FavoriteItemOperation::Add,
                    2 => FavoriteItemOperation::Remove,
                    _ => {
                        return Err(ItemStateProtocolError::InvalidFavoriteOperation {
                            index,
                            actual,
                        });
                    }
                };
                changes.push(FavoriteItemChange::new(item, operation));
            }
            ParsedItemStateFields::FavoriteUpdate(changes)
        }
    };
    ensure_exhausted(&reader, kind.request_name())?;
    Ok(ParsedItemStateRequest { kind, fields })
}

/// Serializes a bounded `PrFavoriteItemGet` snapshot.
///
/// The record's final byte is zero in both the stock codec projection and the
/// C# list response. Delete and unlock intentionally have no corresponding
/// serializer here because their reply objects are consumer-side success
/// capabilities, not failure envelopes.
pub fn serialize_favorite_item_list(
    items: &[FavoriteItemKey],
    maximum_payload: usize,
) -> Result<Vec<u8>, ItemStateProtocolError> {
    if maximum_payload < FAVORITE_ITEM_LIST_HEADER_LENGTH {
        return Err(ItemStateProtocolError::FavoriteListPayloadTooSmall {
            minimum_payload: FAVORITE_ITEM_LIST_HEADER_LENGTH,
            maximum_payload,
        });
    }
    let maximum_records = favorite_item_list_capacity(maximum_payload);
    if items.len() > maximum_records {
        return Err(ItemStateProtocolError::FavoriteListPayloadLimitExceeded {
            count: items.len(),
            maximum_records,
            maximum_payload,
        });
    }
    let count = u32::try_from(items.len()).map_err(|_| {
        ItemStateProtocolError::FavoriteListPayloadLimitExceeded {
            count: items.len(),
            maximum_records,
            maximum_payload,
        }
    })?;
    let mut packet = PacketWriter::named(FAVORITE_ITEM_GET_REPLY_NAME);
    packet.write_u32(count);
    for item in items {
        packet.write_u16(item.category());
        packet.write_u16(item.item_id());
        packet.write_u16(item.serial());
        packet.write_u8(0);
    }
    Ok(packet.into_inner())
}

/// Returns the greatest favorite-list record count that fits one payload.
#[must_use]
pub const fn favorite_item_list_capacity(maximum_payload: usize) -> usize {
    maximum_payload.saturating_sub(FAVORITE_ITEM_LIST_HEADER_LENGTH)
        / FAVORITE_ITEM_LIST_RECORD_LENGTH
}

fn parse_empty_producer_auth(
    reader: &mut PacketReader<'_>,
    name: &'static str,
) -> Result<(), ItemStateProtocolError> {
    let auth_scalar = reader.read_u32()?;
    if auth_scalar != 0 {
        return Err(ItemStateProtocolError::NonZeroProducerAuthScalar {
            name,
            actual: auth_scalar,
        });
    }
    let credential_count = reader.read_u32()?;
    if credential_count != 0 {
        return Err(ItemStateProtocolError::NonEmptyProducerCredentials {
            name,
            count: credential_count,
        });
    }
    Ok(())
}

fn ensure_exhausted(
    reader: &PacketReader<'_>,
    name: &'static str,
) -> Result<(), ItemStateProtocolError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(ItemStateProtocolError::TrailingBytes {
            name,
            count: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use sha2::{Digest, Sha256};

    use super::{
        DELETE_ITEM_REPLY_HASH, DELETE_ITEM_REPLY_NAME, DELETE_ITEM_REQUEST_HASH,
        DELETE_ITEM_REQUEST_NAME, FAVORITE_ITEM_GET_REPLY_HASH, FAVORITE_ITEM_GET_REPLY_NAME,
        FAVORITE_ITEM_GET_REQUEST_HASH, FAVORITE_ITEM_GET_REQUEST_NAME,
        FAVORITE_ITEM_UPDATE_REQUEST_HASH, FAVORITE_ITEM_UPDATE_REQUEST_NAME, FavoriteItemChange,
        FavoriteItemKey, FavoriteItemOperation, ItemStateProtocolError, ItemStateRequest,
        MAX_FAVORITE_ITEM_UPDATE_RECORDS, UNLOCK_ITEM_REPLY_HASH, UNLOCK_ITEM_REPLY_NAME,
        UNLOCK_ITEM_REQUEST_HASH, UNLOCK_ITEM_REQUEST_NAME, classify_item_state_request,
        favorite_item_list_capacity, parse_item_state_request, serialize_favorite_item_list,
    };
    use crate::{
        adler32,
        frame::DEFAULT_MAX_PAYLOAD,
        packet::{PacketError, PacketWriter},
    };

    const HASHES: [(&str, u32); 7] = [
        (DELETE_ITEM_REQUEST_NAME, DELETE_ITEM_REQUEST_HASH),
        (DELETE_ITEM_REPLY_NAME, DELETE_ITEM_REPLY_HASH),
        (UNLOCK_ITEM_REQUEST_NAME, UNLOCK_ITEM_REQUEST_HASH),
        (UNLOCK_ITEM_REPLY_NAME, UNLOCK_ITEM_REPLY_HASH),
        (
            FAVORITE_ITEM_GET_REQUEST_NAME,
            FAVORITE_ITEM_GET_REQUEST_HASH,
        ),
        (FAVORITE_ITEM_GET_REPLY_NAME, FAVORITE_ITEM_GET_REPLY_HASH),
        (
            FAVORITE_ITEM_UPDATE_REQUEST_NAME,
            FAVORITE_ITEM_UPDATE_REQUEST_HASH,
        ),
    ];

    const REQUESTS: [ItemStateRequest; 4] = [
        ItemStateRequest::DeleteItem,
        ItemStateRequest::UnlockItem,
        ItemStateRequest::FavoriteItemGet,
        ItemStateRequest::FavoriteItemUpdate,
    ];

    fn delete_request(category: u16, item_id: u16, serial: u16, quantity_or_mode: u16) -> Vec<u8> {
        let mut packet = PacketWriter::named(DELETE_ITEM_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_u32(0);
        packet.write_u16(category);
        packet.write_u16(item_id);
        packet.write_u16(serial);
        packet.write_u16(quantity_or_mode);
        packet.into_inner()
    }

    fn unlock_request() -> Vec<u8> {
        let mut packet = PacketWriter::named(UNLOCK_ITEM_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_u32(0);
        packet.write_u8(0);
        packet.into_inner()
    }

    fn favorite_get_request() -> Vec<u8> {
        PacketWriter::named(FAVORITE_ITEM_GET_REQUEST_NAME).into_inner()
    }

    fn favorite_update_request(scope: u8, records: &[(FavoriteItemKey, u8)]) -> Vec<u8> {
        let mut packet = PacketWriter::named(FAVORITE_ITEM_UPDATE_REQUEST_NAME);
        packet.write_u8(scope);
        packet.write_u32(u32::try_from(records.len()).expect("test count fits"));
        for (item, operation) in records {
            packet.write_u16(item.category());
            packet.write_u16(item.item_id());
            packet.write_u16(item.serial());
            packet.write_u8(*operation);
        }
        packet.into_inner()
    }

    fn sha256_hex(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn packet_names_match_all_exact_p5136_hashes() {
        for (name, expected) in HASHES {
            assert_eq!(adler32::packet_hash(name), expected, "{name}");
        }
    }

    #[test]
    fn classifier_and_reply_pairing_cover_only_the_four_requests() {
        fn assert_copy_and_eq<T: Copy + Eq>() {}
        assert_copy_and_eq::<ItemStateRequest>();

        for request in REQUESTS {
            assert_eq!(
                classify_item_state_request(adler32::packet_hash(request.request_name())),
                Some(request)
            );
        }
        assert_eq!(
            ItemStateRequest::DeleteItem.reply_name(),
            Some(DELETE_ITEM_REPLY_NAME)
        );
        assert_eq!(
            ItemStateRequest::UnlockItem.reply_name(),
            Some(UNLOCK_ITEM_REPLY_NAME)
        );
        assert_eq!(
            ItemStateRequest::FavoriteItemGet.reply_name(),
            Some(FAVORITE_ITEM_GET_REPLY_NAME)
        );
        assert_eq!(ItemStateRequest::FavoriteItemUpdate.reply_name(), None);
        for hash in [
            0,
            DELETE_ITEM_REPLY_HASH,
            UNLOCK_ITEM_REPLY_HASH,
            FAVORITE_ITEM_GET_REPLY_HASH,
            u32::MAX,
        ] {
            assert_eq!(classify_item_state_request(hash), None);
        }
    }

    #[test]
    fn stock_request_shapes_match_full_golden_packets() {
        assert_eq!(
            delete_request(3, 1_450, 2, 1),
            [
                0xB8, 0x07, 0x4E, 0x4F, // request hash
                0, 0, 0, 0, // auth type
                0, 0, 0, 0, // credential count
                3, 0, 0xAA, 0x05, 2, 0, 1, 0,
            ]
        );
        assert_eq!(
            unlock_request(),
            [0x65, 0x05, 0xC0, 0x27, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
        assert_eq!(favorite_get_request(), [0xB0, 0x06, 0xAD, 0x3B]);
        assert_eq!(
            favorite_update_request(
                1,
                &[
                    (FavoriteItemKey::new(3, 1_450, 2), 1),
                    (FavoriteItemKey::new(u16::MAX, 0, u16::MAX), 2),
                ],
            ),
            [
                0xF3, 0x07, 0x78, 0x52, // request hash
                1,    // producer scope
                2, 0, 0, 0, // raw u32 count
                3, 0, 0xAA, 0x05, 2, 0, 1, // add
                0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 2, // remove
            ]
        );
    }

    #[test]
    fn exact_requests_preserve_delete_and_favorite_fields() {
        let delete = parse_item_state_request(&delete_request(3, 1_450, u16::MAX, 0xBEEF))
            .expect("exact delete");
        assert_eq!(delete.kind(), ItemStateRequest::DeleteItem);
        let fields = delete.delete_fields().expect("delete fields");
        assert_eq!(fields.item(), FavoriteItemKey::new(3, 1_450, u16::MAX));
        assert_eq!(fields.quantity_or_mode(), 0xBEEF);
        assert_eq!(delete.favorite_changes(), None);

        for (fixture, expected) in [
            (unlock_request(), ItemStateRequest::UnlockItem),
            (favorite_get_request(), ItemStateRequest::FavoriteItemGet),
        ] {
            let parsed = parse_item_state_request(&fixture).expect("exact request");
            assert_eq!(parsed.kind(), expected);
            assert_eq!(parsed.delete_fields(), None);
            assert_eq!(parsed.favorite_changes(), None);
        }

        let add = FavoriteItemKey::new(3, 1_450, 2);
        let remove = FavoriteItemKey::new(4, 300, 7);
        let update =
            parse_item_state_request(&favorite_update_request(1, &[(add, 1), (remove, 2)]))
                .expect("exact update");
        assert_eq!(update.kind(), ItemStateRequest::FavoriteItemUpdate);
        assert_eq!(
            update.favorite_changes(),
            Some(
                [
                    FavoriteItemChange::new(add, FavoriteItemOperation::Add),
                    FavoriteItemChange::new(remove, FavoriteItemOperation::Remove),
                ]
                .as_slice()
            )
        );
        assert_eq!(
            update
                .into_favorite_changes()
                .expect("typed update exposes owned changes"),
            vec![
                FavoriteItemChange::new(add, FavoriteItemOperation::Add),
                FavoriteItemChange::new(remove, FavoriteItemOperation::Remove),
            ]
        );
        let get = parse_item_state_request(&favorite_get_request()).expect("exact get");
        assert!(matches!(
            get.into_favorite_changes(),
            Err(ItemStateProtocolError::ParsedKindMismatch {
                expected: ItemStateRequest::FavoriteItemUpdate,
                actual: ItemStateRequest::FavoriteItemGet
            })
        ));
    }

    #[test]
    fn every_truncated_prefix_of_each_exact_request_is_rejected() {
        let fixtures = [
            delete_request(3, 1_450, 2, 1),
            unlock_request(),
            favorite_get_request(),
            favorite_update_request(
                1,
                &[
                    (FavoriteItemKey::new(3, 1_450, 2), 1),
                    (FavoriteItemKey::new(4, 300, 7), 2),
                ],
            ),
        ];
        for fixture in fixtures {
            for length in 0..fixture.len() {
                assert!(
                    matches!(
                        parse_item_state_request(&fixture[..length]),
                        Err(ItemStateProtocolError::Packet(
                            PacketError::Truncated { .. }
                        ))
                    ),
                    "prefix {length} of {} unexpectedly parsed",
                    fixture.len()
                );
            }
        }
    }

    #[test]
    fn unknown_hash_is_rejected_before_body_parsing() {
        for hash in [DELETE_ITEM_REPLY_HASH, 0xDEAD_BEEF] {
            assert!(matches!(
                parse_item_state_request(&hash.to_le_bytes()),
                Err(ItemStateProtocolError::UnsupportedPacketHash { actual })
                    if actual == hash
            ));
        }
    }

    #[test]
    fn producer_auth_scope_and_operation_invariants_have_typed_errors() {
        let mut delete_auth = delete_request(3, 1_450, 2, 1);
        delete_auth[4..8].copy_from_slice(&7_u32.to_le_bytes());
        assert!(matches!(
            parse_item_state_request(&delete_auth),
            Err(ItemStateProtocolError::NonZeroProducerAuthScalar {
                name: DELETE_ITEM_REQUEST_NAME,
                actual: 7
            })
        ));

        let mut unlock_credentials = unlock_request();
        unlock_credentials[8..12].copy_from_slice(&1_u32.to_le_bytes());
        assert!(matches!(
            parse_item_state_request(&unlock_credentials),
            Err(ItemStateProtocolError::NonEmptyProducerCredentials {
                name: UNLOCK_ITEM_REQUEST_NAME,
                count: 1
            })
        ));

        let mut unlock_terminal = unlock_request();
        unlock_terminal[12] = 1;
        assert!(matches!(
            parse_item_state_request(&unlock_terminal),
            Err(ItemStateProtocolError::NonZeroUnlockProducerByte { actual: 1 })
        ));

        assert!(matches!(
            parse_item_state_request(&favorite_update_request(0, &[])),
            Err(ItemStateProtocolError::InvalidFavoriteUpdateScope { actual: 0 })
        ));
        assert!(matches!(
            parse_item_state_request(&favorite_update_request(
                1,
                &[(FavoriteItemKey::new(3, 1_450, 2), 3)]
            )),
            Err(ItemStateProtocolError::InvalidFavoriteOperation {
                index: 0,
                actual: 3
            })
        ));
    }

    #[test]
    fn repeated_item_keys_are_preserved_in_wire_order() {
        let repeated = FavoriteItemKey::new(3, 1_450, 2);
        let parsed = parse_item_state_request(&favorite_update_request(
            1,
            &[(repeated, 1), (repeated, 2), (repeated, 1)],
        ))
        .expect("stock does not establish a per-batch uniqueness invariant");

        assert_eq!(
            parsed.favorite_changes(),
            Some(
                [
                    FavoriteItemChange::new(repeated, FavoriteItemOperation::Add),
                    FavoriteItemChange::new(repeated, FavoriteItemOperation::Remove),
                    FavoriteItemChange::new(repeated, FavoriteItemOperation::Add),
                ]
                .as_slice()
            )
        );
    }

    #[test]
    fn favorite_update_count_is_bounded_before_record_allocation() {
        let mut packet = PacketWriter::named(FAVORITE_ITEM_UPDATE_REQUEST_NAME);
        packet.write_u8(1);
        packet.write_u32(
            u32::try_from(MAX_FAVORITE_ITEM_UPDATE_RECORDS + 1).expect("constant fits u32"),
        );
        assert!(matches!(
            parse_item_state_request(packet.as_slice()),
            Err(ItemStateProtocolError::FavoriteUpdateCountLimitExceeded {
                count,
                maximum: MAX_FAVORITE_ITEM_UPDATE_RECORDS
            }) if count == u32::try_from(MAX_FAVORITE_ITEM_UPDATE_RECORDS + 1).unwrap()
        ));

        let records = (0..MAX_FAVORITE_ITEM_UPDATE_RECORDS)
            .map(|index| {
                (
                    FavoriteItemKey::new(3, u16::try_from(index).expect("test index fits"), 2),
                    1,
                )
            })
            .collect::<Vec<_>>();
        let parsed = parse_item_state_request(&favorite_update_request(1, &records))
            .expect("maximum update");
        assert_eq!(
            parsed.favorite_changes().map(<[FavoriteItemChange]>::len),
            Some(MAX_FAVORITE_ITEM_UPDATE_RECORDS)
        );
    }

    #[test]
    fn cross_kind_body_drift_and_trailing_bytes_are_rejected() {
        let mut delete_as_get = delete_request(3, 1_450, 2, 1);
        delete_as_get[..4].copy_from_slice(&FAVORITE_ITEM_GET_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_item_state_request(&delete_as_get),
            Err(ItemStateProtocolError::TrailingBytes {
                name: FAVORITE_ITEM_GET_REQUEST_NAME,
                count: 16
            })
        ));

        let mut get_as_delete = favorite_get_request();
        get_as_delete[..4].copy_from_slice(&DELETE_ITEM_REQUEST_HASH.to_le_bytes());
        assert!(matches!(
            parse_item_state_request(&get_as_delete),
            Err(ItemStateProtocolError::Packet(
                PacketError::Truncated { .. }
            ))
        ));

        for mut request in [
            delete_request(3, 1_450, 2, 1),
            unlock_request(),
            favorite_get_request(),
            favorite_update_request(1, &[]),
        ] {
            request.extend_from_slice(&[0xA5, 0x5A]);
            assert!(matches!(
                parse_item_state_request(&request),
                Err(ItemStateProtocolError::TrailingBytes { count: 2, .. })
            ));
        }
    }

    #[test]
    fn favorite_list_reply_matches_empty_and_nonempty_goldens() {
        let empty = serialize_favorite_item_list(&[], DEFAULT_MAX_PAYLOAD).expect("empty list");
        assert_eq!(empty, [0xB1, 0x06, 0xBD, 0x3B, 0, 0, 0, 0]);
        assert_eq!(
            sha256_hex(&empty),
            "4ad640170c11b488eb2d905bbb10805adadd9db90b793fe60e2dbfff4c66dcbf"
        );

        let one =
            serialize_favorite_item_list(&[FavoriteItemKey::new(1, 2, 3)], DEFAULT_MAX_PAYLOAD)
                .expect("one item");
        assert_eq!(
            one,
            [0xB1, 0x06, 0xBD, 0x3B, 1, 0, 0, 0, 1, 0, 2, 0, 3, 0, 0]
        );
        assert_eq!(
            sha256_hex(&one),
            "284c1a6cd7e59d2de85988a505c06fac149faaf6a7c5acd001b62659d184ef9f"
        );

        let reply = serialize_favorite_item_list(
            &[
                FavoriteItemKey::new(3, 1_450, 2),
                FavoriteItemKey::new(u16::MAX, 0, u16::MAX),
            ],
            DEFAULT_MAX_PAYLOAD,
        )
        .expect("bounded list");
        assert_eq!(
            reply,
            [
                0xB1, 0x06, 0xBD, 0x3B, // reply hash
                2, 0, 0, 0, // count
                3, 0, 0xAA, 0x05, 2, 0, 0, // first item
                0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, // second item
            ]
        );
        assert_eq!(
            sha256_hex(&reply),
            "816a87043d9494e065a93c336cab2fe515840edf6d2064e9bbd06da1b14b200b"
        );
    }

    #[test]
    fn favorite_list_reply_enforces_the_configured_payload_cap() {
        const TEST_MAXIMUM_PAYLOAD: usize = 29;
        let maximum = vec![FavoriteItemKey::new(3, 1_450, 2); 3];
        let reply =
            serialize_favorite_item_list(&maximum, TEST_MAXIMUM_PAYLOAD).expect("maximum list");
        assert_eq!(reply.len(), TEST_MAXIMUM_PAYLOAD);

        let excessive = vec![FavoriteItemKey::new(3, 1_450, 2); 4];
        assert!(matches!(
            serialize_favorite_item_list(&excessive, TEST_MAXIMUM_PAYLOAD),
            Err(ItemStateProtocolError::FavoriteListPayloadLimitExceeded {
                count: 4,
                maximum_records: 3,
                maximum_payload: TEST_MAXIMUM_PAYLOAD
            })
        ));
        assert_eq!(favorite_item_list_capacity(7), 0);
        assert_eq!(favorite_item_list_capacity(8), 0);
        assert_eq!(favorite_item_list_capacity(15), 1);
        assert!(matches!(
            serialize_favorite_item_list(&[], 7),
            Err(ItemStateProtocolError::FavoriteListPayloadTooSmall {
                minimum_payload: 8,
                maximum_payload: 7
            })
        ));
        assert_eq!(
            serialize_favorite_item_list(&[], 8).expect("header exactly fits"),
            [0xB1, 0x06, 0xBD, 0x3B, 0, 0, 0, 0]
        );
        assert_eq!(
            favorite_item_list_capacity(DEFAULT_MAX_PAYLOAD),
            super::DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS
        );
    }
}
