//! P5136 messenger TCP logical packets.

use thiserror::Error;

use crate::{
    adler32,
    packet::{PacketError, PacketReader, PacketWriter},
};

pub const DEFAULT_MAX_MESSENGER_STRING_UNITS: usize = 4_096;
pub const MESSENGER_FRAME_HEADER_LENGTH: usize = 4;
pub const MIN_MESSENGER_PAYLOAD_LENGTH: usize = 4;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MessengerFrameError {
    #[error("messenger frame has a negative payload length {0}")]
    NegativePayloadLength(i32),

    #[error(
        "messenger payload length {length} is shorter than the required packet hash ({minimum})"
    )]
    PayloadTooShort { length: usize, minimum: usize },

    #[error("messenger payload length {length} exceeds configured maximum {maximum}")]
    PayloadTooLarge { length: usize, maximum: usize },

    #[error("messenger payload length does not fit in the signed P5136 i32 prefix")]
    PayloadLengthOverflow,

    #[error("messenger frame header is truncated: expected 4 bytes, received {actual}")]
    TruncatedHeader { actual: usize },

    #[error("messenger frame length mismatch: expected {expected} bytes, received {actual}")]
    FrameLengthMismatch { expected: usize, actual: usize },
}

#[derive(Debug, Error)]
pub enum MessengerError {
    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error("unknown messenger packet hash 0x{0:08X}")]
    UnknownPacket(u32),

    #[error("{packet} carries {length} unexpected trailing bytes")]
    TrailingBytes { packet: &'static str, length: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessengerRequest {
    EnterChatServer {
        user_no: u32,
        chat_type: u32,
        nickname: String,
    },
    InviteChat {
        inviter_user_no: u32,
        invitee_user_no: u32,
        inviter_nickname: String,
        invitee_nickname: String,
    },
    Chat {
        room_id: u32,
        nickname: String,
        message: String,
    },
    LeaveChat {
        user_no: u32,
        room_id: u32,
    },
    GuildChat {
        nickname: String,
        message: String,
    },
}

/// Decodes the signed little-endian length prefix used by P5136 messenger TCP.
///
/// The returned length excludes the four-byte prefix and includes the logical
/// packet hash.
pub fn decode_frame_length(
    header: [u8; MESSENGER_FRAME_HEADER_LENGTH],
    maximum: usize,
) -> Result<usize, MessengerFrameError> {
    let signed = i32::from_le_bytes(header);
    let length =
        usize::try_from(signed).map_err(|_| MessengerFrameError::NegativePayloadLength(signed))?;
    validate_frame_payload_length(length, maximum)?;
    Ok(length)
}

/// Encodes one complete unencrypted P5136 messenger TCP frame.
pub fn encode_frame(payload: &[u8], maximum: usize) -> Result<Vec<u8>, MessengerFrameError> {
    validate_frame_payload_length(payload.len(), maximum)?;
    let length =
        i32::try_from(payload.len()).map_err(|_| MessengerFrameError::PayloadLengthOverflow)?;
    let mut frame = Vec::with_capacity(MESSENGER_FRAME_HEADER_LENGTH + payload.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

/// Validates and removes one complete P5136 messenger TCP length prefix.
pub fn decode_frame(frame: &[u8], maximum: usize) -> Result<&[u8], MessengerFrameError> {
    let header =
        frame
            .get(..MESSENGER_FRAME_HEADER_LENGTH)
            .ok_or(MessengerFrameError::TruncatedHeader {
                actual: frame.len(),
            })?;
    let length = decode_frame_length(
        header
            .try_into()
            .expect("a four-byte slice always converts to a four-byte array"),
        maximum,
    )?;
    let expected = MESSENGER_FRAME_HEADER_LENGTH + length;
    if frame.len() != expected {
        return Err(MessengerFrameError::FrameLengthMismatch {
            expected,
            actual: frame.len(),
        });
    }
    Ok(&frame[MESSENGER_FRAME_HEADER_LENGTH..])
}

fn validate_frame_payload_length(length: usize, maximum: usize) -> Result<(), MessengerFrameError> {
    if length < MIN_MESSENGER_PAYLOAD_LENGTH {
        return Err(MessengerFrameError::PayloadTooShort {
            length,
            minimum: MIN_MESSENGER_PAYLOAD_LENGTH,
        });
    }
    if length > maximum {
        return Err(MessengerFrameError::PayloadTooLarge { length, maximum });
    }
    if i32::try_from(length).is_err() {
        return Err(MessengerFrameError::PayloadLengthOverflow);
    }
    Ok(())
}

pub fn parse_request(
    packet: &[u8],
    maximum_string_units: usize,
) -> Result<MessengerRequest, MessengerError> {
    let mut reader = PacketReader::new(packet);
    let hash = reader.read_u32()?;

    if hash == adler32::packet_hash("PqEnterChatServer") {
        let request = MessengerRequest::EnterChatServer {
            user_no: reader.read_u32()?,
            chat_type: reader.read_u32()?,
            nickname: reader.read_utf16_bounded(maximum_string_units)?,
        };
        finish(reader, "PqEnterChatServer")?;
        return Ok(request);
    }

    if hash == adler32::packet_hash("PqInitInviteMsgrChat") {
        let request = MessengerRequest::InviteChat {
            inviter_user_no: reader.read_u32()?,
            invitee_user_no: reader.read_u32()?,
            inviter_nickname: reader.read_utf16_bounded(maximum_string_units)?,
            invitee_nickname: reader.read_utf16_bounded(maximum_string_units)?,
        };
        finish(reader, "PqInitInviteMsgrChat")?;
        return Ok(request);
    }

    if hash == adler32::packet_hash("PqMsgrChat") {
        let request = MessengerRequest::Chat {
            room_id: reader.read_u32()?,
            nickname: reader.read_utf16_bounded(maximum_string_units)?,
            message: reader.read_utf16_bounded(maximum_string_units)?,
        };
        finish(reader, "PqMsgrChat")?;
        return Ok(request);
    }

    if hash == adler32::packet_hash("PqLeaveMsgrChat") {
        let request = MessengerRequest::LeaveChat {
            user_no: reader.read_u32()?,
            room_id: reader.read_u32()?,
        };
        finish(reader, "PqLeaveMsgrChat")?;
        return Ok(request);
    }

    if hash == adler32::packet_hash("PqGuildChat") {
        let request = MessengerRequest::GuildChat {
            nickname: reader.read_utf16_bounded(maximum_string_units)?,
            message: reader.read_utf16_bounded(maximum_string_units)?,
        };
        finish(reader, "PqGuildChat")?;
        return Ok(request);
    }

    Err(MessengerError::UnknownPacket(hash))
}

pub fn serialize_invite_chat(
    source_user_no: u32,
    target_user_no: u32,
    source_nickname: &str,
    target_nickname: &str,
    room_id: u32,
) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrInitInviteMsgrChat");
    packet.write_u32(source_user_no);
    packet.write_u32(target_user_no);
    packet.write_utf16(source_nickname)?;
    packet.write_utf16(target_nickname)?;
    packet.write_u32(room_id);
    packet.write_i32(0);
    Ok(packet.into_inner())
}

pub fn serialize_chat(
    room_id: u32,
    sender_user_no: u32,
    nickname: &str,
    message: &str,
) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrMsgrChat");
    packet.write_u32(room_id);
    packet.write_u32(sender_user_no);
    packet.write_utf16(nickname)?;
    packet.write_utf16(message)?;
    packet.write_i32(0);
    Ok(packet.into_inner())
}

#[must_use]
pub fn serialize_leave_chat(user_no: u32, room_id: u32) -> Vec<u8> {
    let mut packet = PacketWriter::named("PrLeaveMsgrChat");
    packet.write_u32(user_no);
    packet.write_u32(room_id);
    packet.into_inner()
}

pub fn serialize_guild_chat(nickname: &str, message: &str) -> Result<Vec<u8>, PacketError> {
    let mut packet = PacketWriter::named("PrGuildChat");
    packet.write_utf16(nickname)?;
    packet.write_utf16(message)?;
    Ok(packet.into_inner())
}

fn finish(reader: PacketReader<'_>, packet: &'static str) -> Result<(), MessengerError> {
    if reader.remaining().is_empty() {
        Ok(())
    } else {
        Err(MessengerError::TrailingBytes {
            packet,
            length: reader.remaining().len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use crate::{
        adler32,
        packet::{PacketReader, PacketWriter},
    };

    use super::{
        MessengerError, MessengerFrameError, MessengerRequest, decode_frame, decode_frame_length,
        encode_frame, parse_request, serialize_chat, serialize_guild_chat, serialize_invite_chat,
        serialize_leave_chat, validate_frame_payload_length,
    };

    // Fixed outputs from C# OutPacket plus MsgrServer.BeginSend. These do not
    // use the Rust packet writer to construct their expected bytes.
    #[rustfmt::skip]
    const CSHARP_INVITE_FRAME: &[u8] = &[
        0x2E, 0x00, 0x00, 0x00, 0xDE, 0x07, 0xE1, 0x51, 0x11, 0x00, 0x00, 0x00, 0x12, 0x00,
        0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x52, 0x00, 0x69, 0x00, 0x64, 0x00, 0x65, 0x00,
        0x72, 0x00, 0x04, 0x00, 0x00, 0x00, 0x50, 0x00, 0x65, 0x00, 0x65, 0x00, 0x72, 0x00,
        0x09, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    #[rustfmt::skip]
    const CSHARP_CHAT_FRAME: &[u8] = &[
        0x2C, 0x00, 0x00, 0x00, 0xDB, 0x03, 0xCD, 0x14, 0x09, 0x00, 0x00, 0x00, 0x11, 0x00,
        0x00, 0x00, 0x05, 0x00, 0x00, 0x00, 0x52, 0x00, 0x69, 0x00, 0x64, 0x00, 0x65, 0x00,
        0x72, 0x00, 0x05, 0x00, 0x00, 0x00, 0x68, 0x00, 0x65, 0x00, 0x6C, 0x00, 0x6C, 0x00,
        0x6F, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    #[rustfmt::skip]
    const CSHARP_LEAVE_FRAME: &[u8] = &[
        0x0C, 0x00, 0x00, 0x00, 0xC8, 0x05, 0x83, 0x2D, 0x11, 0x00, 0x00, 0x00, 0x09, 0x00,
        0x00, 0x00,
    ];
    #[rustfmt::skip]
    const CSHARP_GUILD_FRAME: &[u8] = &[
        0x20, 0x00, 0x00, 0x00, 0x37, 0x04, 0xE0, 0x18, 0x05, 0x00, 0x00, 0x00, 0x52, 0x00,
        0x69, 0x00, 0x64, 0x00, 0x65, 0x00, 0x72, 0x00, 0x05, 0x00, 0x00, 0x00, 0x68, 0x00,
        0x65, 0x00, 0x6C, 0x00, 0x6C, 0x00, 0x6F, 0x00,
    ];

    #[test]
    fn parses_each_client_request_layout() {
        let mut enter = PacketWriter::named("PqEnterChatServer");
        enter.write_u32(17);
        enter.write_u32(2);
        enter.write_utf16("Rider").unwrap();
        assert_eq!(
            parse_request(enter.as_slice(), 32).unwrap(),
            MessengerRequest::EnterChatServer {
                user_no: 17,
                chat_type: 2,
                nickname: "Rider".to_owned(),
            }
        );

        let mut invite = PacketWriter::named("PqInitInviteMsgrChat");
        invite.write_u32(17);
        invite.write_u32(18);
        invite.write_utf16("Rider").unwrap();
        invite.write_utf16("Peer").unwrap();
        assert_eq!(
            parse_request(invite.as_slice(), 32).unwrap(),
            MessengerRequest::InviteChat {
                inviter_user_no: 17,
                invitee_user_no: 18,
                inviter_nickname: "Rider".to_owned(),
                invitee_nickname: "Peer".to_owned(),
            }
        );

        let mut chat = PacketWriter::named("PqMsgrChat");
        chat.write_u32(9);
        chat.write_utf16("Rider").unwrap();
        chat.write_utf16("hello").unwrap();
        assert_eq!(
            parse_request(chat.as_slice(), 32).unwrap(),
            MessengerRequest::Chat {
                room_id: 9,
                nickname: "Rider".to_owned(),
                message: "hello".to_owned(),
            }
        );

        let mut leave = PacketWriter::named("PqLeaveMsgrChat");
        leave.write_u32(17);
        leave.write_u32(9);
        assert_eq!(
            parse_request(leave.as_slice(), 32).unwrap(),
            MessengerRequest::LeaveChat {
                user_no: 17,
                room_id: 9,
            }
        );

        let mut guild = PacketWriter::named("PqGuildChat");
        guild.write_utf16("Rider").unwrap();
        guild.write_utf16("guild hello").unwrap();
        assert_eq!(
            parse_request(guild.as_slice(), 32).unwrap(),
            MessengerRequest::GuildChat {
                nickname: "Rider".to_owned(),
                message: "guild hello".to_owned(),
            }
        );
    }

    #[test]
    fn replies_match_the_csharp_field_order() {
        let invite = serialize_invite_chat(17, 18, "Rider", "Peer", 9).unwrap();
        let mut reader = PacketReader::new(&invite);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrInitInviteMsgrChat")
        );
        assert_eq!(reader.read_u32().unwrap(), 17);
        assert_eq!(reader.read_u32().unwrap(), 18);
        assert_eq!(reader.read_utf16().unwrap(), "Rider");
        assert_eq!(reader.read_utf16().unwrap(), "Peer");
        assert_eq!(reader.read_u32().unwrap(), 9);
        assert_eq!(reader.read_i32().unwrap(), 0);
        assert!(reader.remaining().is_empty());

        let chat = serialize_chat(9, 17, "Rider", "hello").unwrap();
        let mut reader = PacketReader::new(&chat);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrMsgrChat")
        );
        assert_eq!(reader.read_u32().unwrap(), 9);
        assert_eq!(reader.read_u32().unwrap(), 17);
        assert_eq!(reader.read_utf16().unwrap(), "Rider");
        assert_eq!(reader.read_utf16().unwrap(), "hello");
        assert_eq!(reader.read_i32().unwrap(), 0);
        assert!(reader.remaining().is_empty());

        let leave = serialize_leave_chat(17, 9);
        assert_eq!(
            leave,
            [
                adler32::packet_hash("PrLeaveMsgrChat").to_le_bytes(),
                17_u32.to_le_bytes(),
                9_u32.to_le_bytes(),
            ]
            .concat()
        );

        let guild = serialize_guild_chat("Rider", "hello").unwrap();
        let mut reader = PacketReader::new(&guild);
        assert_eq!(
            reader.read_u32().unwrap(),
            adler32::packet_hash("PrGuildChat")
        );
        assert_eq!(reader.read_utf16().unwrap(), "Rider");
        assert_eq!(reader.read_utf16().unwrap(), "hello");
        assert!(reader.remaining().is_empty());
    }

    #[test]
    fn rejects_oversized_strings_and_trailing_bytes() {
        let mut enter = PacketWriter::named("PqEnterChatServer");
        enter.write_u32(17);
        enter.write_u32(0);
        enter.write_utf16("too-long").unwrap();
        assert!(matches!(
            parse_request(enter.as_slice(), 3),
            Err(MessengerError::Packet(
                crate::packet::PacketError::StringLimitExceeded { .. }
            ))
        ));

        let mut leave = PacketWriter::named("PqLeaveMsgrChat");
        leave.write_u32(17);
        leave.write_u32(9);
        leave.write_u8(0);
        assert!(matches!(
            parse_request(leave.as_slice(), 32),
            Err(MessengerError::TrailingBytes {
                packet: "PqLeaveMsgrChat",
                length: 1,
            })
        ));
    }

    #[test]
    fn full_wire_frames_match_csharp_fixtures() {
        let invite = serialize_invite_chat(17, 18, "Rider", "Peer", 9).unwrap();
        assert_eq!(encode_frame(&invite, 1_024).unwrap(), CSHARP_INVITE_FRAME);

        let chat = serialize_chat(9, 17, "Rider", "hello").unwrap();
        assert_eq!(encode_frame(&chat, 1_024).unwrap(), CSHARP_CHAT_FRAME);

        let leave = serialize_leave_chat(17, 9);
        assert_eq!(encode_frame(&leave, 1_024).unwrap(), CSHARP_LEAVE_FRAME);

        let guild = serialize_guild_chat("Rider", "hello").unwrap();
        assert_eq!(encode_frame(&guild, 1_024).unwrap(), CSHARP_GUILD_FRAME);

        assert_eq!(decode_frame(CSHARP_INVITE_FRAME, 1_024).unwrap(), invite);
    }

    #[test]
    fn signed_length_bounds_are_checked_before_body_allocation() {
        assert_eq!(
            decode_frame_length((-1_i32).to_le_bytes(), 1_024),
            Err(MessengerFrameError::NegativePayloadLength(-1))
        );
        assert_eq!(
            decode_frame_length(3_i32.to_le_bytes(), 1_024),
            Err(MessengerFrameError::PayloadTooShort {
                length: 3,
                minimum: 4,
            })
        );
        assert_eq!(
            decode_frame_length(1_025_i32.to_le_bytes(), 1_024),
            Err(MessengerFrameError::PayloadTooLarge {
                length: 1_025,
                maximum: 1_024,
            })
        );
        let overflow = usize::try_from(i32::MAX).unwrap() + 1;
        assert_eq!(
            validate_frame_payload_length(overflow, usize::MAX),
            Err(MessengerFrameError::PayloadLengthOverflow)
        );
    }

    #[test]
    fn complete_frame_decoder_rejects_truncation_and_coalescing() {
        assert_eq!(
            decode_frame(&[4, 0, 0], 1_024),
            Err(MessengerFrameError::TruncatedHeader { actual: 3 })
        );
        assert_eq!(
            decode_frame(&[4, 0, 0, 0, 1, 2, 3], 1_024),
            Err(MessengerFrameError::FrameLengthMismatch {
                expected: 8,
                actual: 7,
            })
        );
        assert_eq!(
            decode_frame(&[4, 0, 0, 0, 1, 2, 3, 4, 5], 1_024),
            Err(MessengerFrameError::FrameLengthMismatch {
                expected: 8,
                actual: 9,
            })
        );
    }
}
