//! Bounded P5136 TCP framing.

use thiserror::Error;

use crate::crypto;

pub const HEADER_XOR: u32 = 4_164_199_944;
pub const CHECKSUM_XOR: u32 = 3_388_492_432;
pub const IV_INCREMENT: u32 = 21_446_425;
pub const DEFAULT_MAX_PAYLOAD: usize = 1_048_576;

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum FrameError {
    #[error("payload length {length} exceeds configured maximum {maximum}")]
    PayloadTooLarge { length: usize, maximum: usize },

    #[error("payload length does not fit in the P5136 u32 header")]
    PayloadLengthOverflow,

    #[error("frame is truncated: expected {expected} bytes, received {actual}")]
    Truncated { expected: usize, actual: usize },

    #[error("encrypted frame declares {declared} bytes; at least four checksum bytes are required")]
    InvalidEncryptedLength { declared: u32 },

    #[error("frame length mismatch: header requires {expected} bytes, received {actual}")]
    LengthMismatch { expected: usize, actual: usize },

    #[error("encrypted frame checksum mismatch")]
    ChecksumMismatch,
}

#[must_use]
pub fn advance_iv(iv: u32) -> u32 {
    let next = iv.wrapping_add(IV_INCREMENT);
    if next == 0 { 1 } else { next }
}

pub fn encode_plain(payload: &[u8], maximum: usize) -> Result<Vec<u8>, FrameError> {
    validate_payload_length(payload.len(), maximum)?;
    let length = u32::try_from(payload.len()).map_err(|_| FrameError::PayloadLengthOverflow)?;
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(payload);
    Ok(frame)
}

pub fn decode_plain(frame: &[u8], maximum: usize) -> Result<&[u8], FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::Truncated {
            expected: 4,
            actual: frame.len(),
        });
    }

    let length = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    let payload_length = usize::try_from(length).map_err(|_| FrameError::PayloadLengthOverflow)?;
    validate_payload_length(payload_length, maximum)?;
    let expected = payload_length
        .checked_add(4)
        .ok_or(FrameError::PayloadLengthOverflow)?;
    if frame.len() != expected {
        return Err(FrameError::LengthMismatch {
            expected,
            actual: frame.len(),
        });
    }
    Ok(&frame[4..])
}

/// Returns the number of bytes following an encrypted frame's four-byte
/// header. This includes the trailing four-byte checksum.
pub fn encrypted_body_length(
    encoded_header: u32,
    iv: u32,
    maximum: usize,
) -> Result<usize, FrameError> {
    let declared = iv ^ encoded_header ^ HEADER_XOR;
    if declared < 4 {
        return Err(FrameError::InvalidEncryptedLength { declared });
    }

    let payload_length =
        usize::try_from(declared - 4).map_err(|_| FrameError::PayloadLengthOverflow)?;
    validate_payload_length(payload_length, maximum)?;
    usize::try_from(declared).map_err(|_| FrameError::PayloadLengthOverflow)
}

pub fn encode_encrypted(
    payload: &[u8],
    iv: &mut u32,
    maximum: usize,
) -> Result<Vec<u8>, FrameError> {
    validate_payload_length(payload.len(), maximum)?;
    let payload_length =
        u32::try_from(payload.len()).map_err(|_| FrameError::PayloadLengthOverflow)?;
    let declared = payload_length
        .checked_add(4)
        .ok_or(FrameError::PayloadLengthOverflow)?;
    let active_iv = *iv;
    let encoded_header = active_iv ^ declared ^ HEADER_XOR;

    let mut encrypted = payload.to_vec();
    let checksum = crypto::encrypt_in_place(&mut encrypted, active_iv);
    let encoded_checksum = active_iv ^ checksum ^ CHECKSUM_XOR;

    let mut frame = Vec::with_capacity(payload.len() + 8);
    frame.extend_from_slice(&encoded_header.to_le_bytes());
    frame.extend_from_slice(&encrypted);
    frame.extend_from_slice(&encoded_checksum.to_le_bytes());
    *iv = advance_iv(active_iv);
    Ok(frame)
}

pub fn decode_encrypted(frame: &[u8], iv: &mut u32, maximum: usize) -> Result<Vec<u8>, FrameError> {
    if frame.len() < 4 {
        return Err(FrameError::Truncated {
            expected: 4,
            actual: frame.len(),
        });
    }

    let active_iv = *iv;
    let encoded_header = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]);
    let body_length = encrypted_body_length(encoded_header, active_iv, maximum)?;
    let expected = body_length
        .checked_add(4)
        .ok_or(FrameError::PayloadLengthOverflow)?;
    if frame.len() != expected {
        return Err(FrameError::LengthMismatch {
            expected,
            actual: frame.len(),
        });
    }

    let payload_end = frame.len() - 4;
    let mut payload = frame[4..payload_end].to_vec();
    let encoded_checksum = u32::from_le_bytes([
        frame[payload_end],
        frame[payload_end + 1],
        frame[payload_end + 2],
        frame[payload_end + 3],
    ]);
    let actual_checksum = crypto::decrypt_in_place(&mut payload, active_iv);
    let expected_checksum = active_iv ^ encoded_checksum ^ CHECKSUM_XOR;
    if actual_checksum != expected_checksum {
        return Err(FrameError::ChecksumMismatch);
    }

    *iv = advance_iv(active_iv);
    Ok(payload)
}

fn validate_payload_length(length: usize, maximum: usize) -> Result<(), FrameError> {
    if length > maximum {
        Err(FrameError::PayloadTooLarge { length, maximum })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DEFAULT_MAX_PAYLOAD, FrameError, decode_encrypted, decode_plain, encode_encrypted,
        encode_plain, encrypted_body_length,
    };

    #[test]
    fn plaintext_frame_round_trips() {
        let frame = encode_plain(b"hello", DEFAULT_MAX_PAYLOAD).unwrap();
        assert_eq!(frame, [5, 0, 0, 0, b'h', b'e', b'l', b'l', b'o']);
        assert_eq!(decode_plain(&frame, DEFAULT_MAX_PAYLOAD).unwrap(), b"hello");
    }

    #[test]
    fn encrypted_frame_matches_csharp_golden_and_round_trips() {
        let payload = (0_u8..21).collect::<Vec<_>>();
        let mut send_iv = 0xa1b7_1c9b;
        let frame = encode_encrypted(&payload, &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();

        assert_eq!(
            frame,
            [
                0x8a, 0xba, 0x83, 0x59, 0x53, 0x1a, 0x06, 0xb6, 0x33, 0x0b, 0x0e, 0x2a, 0x52, 0x82,
                0xbe, 0x8e, 0x57, 0x38, 0x04, 0x5d, 0x43, 0x0a, 0x16, 0xa6, 0x23, 0xa9, 0x57, 0x4f,
                0x68,
            ]
        );

        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            decode_encrypted(&frame, &mut receive_iv, DEFAULT_MAX_PAYLOAD).unwrap(),
            payload
        );
        assert_eq!(receive_iv, send_iv);
    }

    #[test]
    fn short_encrypted_frame_matches_independent_p5136_golden() {
        let payload = [0xc8, 0x04, 0x9d, 0x1e, 0x10, 0x32, 0x54, 0x76];
        let mut iv = 0x5136_5136;
        let wire = encode_encrypted(&payload, &mut iv, DEFAULT_MAX_PAYLOAD).unwrap();

        assert_eq!(
            wire,
            [
                0x32, 0xf7, 0x02, 0xa9, 0x36, 0x52, 0x18, 0x5b, 0x8a, 0x71, 0xdd, 0xab, 0xa2, 0x30,
                0xce, 0x98,
            ]
        );
        assert_eq!(iv, 0x527d_904f);
    }

    #[test]
    fn malicious_lengths_are_rejected_before_allocation() {
        let iv = 0xa1b7_1c9b;
        let encoded = iv ^ 0x000f_4244 ^ super::HEADER_XOR;
        assert_eq!(
            encrypted_body_length(encoded, iv, 1_000),
            Err(FrameError::PayloadTooLarge {
                length: 1_000_000,
                maximum: 1_000,
            })
        );
    }

    #[test]
    fn checksum_corruption_does_not_advance_iv() {
        let mut send_iv = 0xa1b7_1c9b;
        let mut frame = encode_encrypted(b"packet", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        let last = frame.len() - 1;
        frame[last] ^= 0x80;

        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            decode_encrypted(&frame, &mut receive_iv, DEFAULT_MAX_PAYLOAD),
            Err(FrameError::ChecksumMismatch)
        );
        assert_eq!(receive_iv, 0xa1b7_1c9b);
    }
}
