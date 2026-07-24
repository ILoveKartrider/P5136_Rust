//! Legacy scalar substitution used by `WriteEnc*`/`ReadEncoded*`.
//!
//! The C# implementation indexes its inverse table with `(position + byte) %
//! 255`, not 256. That makes a small upper range intentionally lossy at each
//! position. We preserve that wire behavior exactly instead of silently
//! "fixing" old clients' encoding.

use thiserror::Error;

/// The original implementation uses a byte loop counter and cannot terminate
/// for buffers longer than 255 bytes.
pub const MAX_ENCODED_SCALAR_BYTES: usize = u8::MAX as usize;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum EncodedScalarError {
    #[error("encoded-scalar buffer length {length} exceeds the P5136 limit of 255 bytes")]
    TooLong { length: usize },
}

/// Encoded byte to pre-position-adjusted plaintext byte.
///
/// This is the first 256 entries of `CryptoConstants.specKeys1`. The C# array
/// carries eight unreachable trailing values left by the legacy source.
const DECODE_TABLE: [u8; 256] = [
    79, 200, 44, 182, 173, 43, 108, 229, 46, 65, 174, 137, 154, 120, 126, 207, 91, 124, 189, 116,
    251, 220, 5, 239, 42, 167, 225, 205, 215, 90, 41, 45, 13, 4, 245, 231, 180, 133, 208, 59, 178,
    20, 110, 150, 219, 99, 203, 243, 141, 160, 81, 195, 31, 78, 228, 57, 181, 121, 76, 170, 21,
    143, 240, 37, 222, 9, 152, 138, 113, 56, 254, 27, 100, 75, 62, 52, 107, 69, 25, 196, 123, 30,
    193, 39, 19, 197, 36, 11, 128, 80, 230, 29, 32, 145, 58, 209, 176, 50, 129, 244, 147, 102, 112,
    151, 97, 70, 40, 168, 96, 83, 190, 86, 2, 51, 177, 217, 172, 105, 118, 38, 210, 206, 201, 183,
    1, 248, 74, 17, 246, 139, 22, 164, 55, 185, 67, 237, 159, 3, 184, 212, 235, 156, 117, 162, 142,
    214, 33, 127, 48, 213, 136, 211, 26, 194, 199, 169, 161, 134, 89, 227, 140, 87, 10, 144, 77,
    247, 187, 125, 82, 241, 23, 103, 16, 95, 192, 85, 6, 68, 149, 66, 165, 8, 253, 61, 157, 224, 0,
    166, 92, 7, 179, 255, 73, 49, 242, 236, 54, 153, 104, 202, 115, 64, 111, 28, 233, 53, 72, 232,
    171, 252, 135, 101, 18, 146, 98, 71, 221, 148, 24, 234, 198, 114, 163, 109, 106, 249, 84, 186,
    238, 94, 223, 155, 132, 175, 60, 88, 226, 35, 122, 15, 34, 63, 204, 158, 218, 14, 130, 131,
    191, 188, 93, 47, 216, 119, 250, 12,
];

const fn invert_decode_table() -> [u8; 256] {
    let mut inverse = [0_u8; 256];
    let mut encoded = 0_u8;
    loop {
        inverse[DECODE_TABLE[encoded as usize] as usize] = encoded;
        if encoded == u8::MAX {
            break;
        }
        encoded += 1;
    }
    inverse
}

const ENCODE_TABLE: [u8; 256] = invert_decode_table();

pub fn encode_bytes(plain: &[u8]) -> Result<Vec<u8>, EncodedScalarError> {
    validate_length(plain.len())?;
    Ok(plain
        .iter()
        .enumerate()
        .map(|(position, value)| {
            let key = (position + usize::from(*value)) % 255;
            ENCODE_TABLE[key]
        })
        .collect())
}

pub fn decode_bytes(encoded: &[u8]) -> Result<Vec<u8>, EncodedScalarError> {
    validate_length(encoded.len())?;
    Ok(encoded
        .iter()
        .enumerate()
        .map(|(position, value)| {
            let position =
                u8::try_from(position).expect("validated encoded scalar position fits in u8");
            DECODE_TABLE[usize::from(*value)].wrapping_sub(position)
        })
        .collect())
}

#[must_use]
pub fn encode_u8(value: u8) -> u8 {
    ENCODE_TABLE[usize::from(value) % 255]
}

#[must_use]
pub fn decode_u8(value: u8) -> u8 {
    DECODE_TABLE[usize::from(value)]
}

#[must_use]
pub fn encode_u16(value: u16) -> [u8; 2] {
    encode_array(value.to_le_bytes())
}

#[must_use]
pub fn decode_u16(value: [u8; 2]) -> u16 {
    u16::from_le_bytes(decode_array(value))
}

#[must_use]
pub fn encode_i32(value: i32) -> [u8; 4] {
    encode_array(value.to_le_bytes())
}

#[must_use]
pub fn decode_i32(value: [u8; 4]) -> i32 {
    i32::from_le_bytes(decode_array(value))
}

#[must_use]
pub fn encode_u32(value: u32) -> [u8; 4] {
    encode_array(value.to_le_bytes())
}

#[must_use]
pub fn decode_u32(value: [u8; 4]) -> u32 {
    u32::from_le_bytes(decode_array(value))
}

#[must_use]
pub fn encode_f32(value: f32) -> [u8; 4] {
    encode_array(value.to_le_bytes())
}

#[must_use]
pub fn decode_f32(value: [u8; 4]) -> f32 {
    f32::from_le_bytes(decode_array(value))
}

fn encode_array<const LENGTH: usize>(plain: [u8; LENGTH]) -> [u8; LENGTH] {
    std::array::from_fn(|position| {
        let key = (position + usize::from(plain[position])) % 255;
        ENCODE_TABLE[key]
    })
}

fn decode_array<const LENGTH: usize>(encoded: [u8; LENGTH]) -> [u8; LENGTH] {
    std::array::from_fn(|position| {
        let position = u8::try_from(position).expect("P5136 scalar positions fit in u8");
        DECODE_TABLE[usize::from(encoded[usize::from(position)])].wrapping_sub(position)
    })
}

fn validate_length(length: usize) -> Result<(), EncodedScalarError> {
    if length > MAX_ENCODED_SCALAR_BYTES {
        return Err(EncodedScalarError::TooLong { length });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        EncodedScalarError, decode_bytes, decode_f32, decode_i32, decode_u16, decode_u32,
        encode_bytes, encode_f32, encode_i32, encode_u16, encode_u32,
    };

    #[test]
    fn matches_csharp_scalar_goldens() {
        assert_eq!(encode_u16(5_136), [0xAC, 0x3C]);
        assert_eq!(decode_u16([0xAC, 0x3C]), 5_136);

        assert_eq!(encode_i32(-123_456_789), [0x8C, 0x71, 0xBB, 0x14]);
        assert_eq!(decode_i32([0x8C, 0x71, 0xBB, 0x14]), -123_456_789);

        assert_eq!(encode_u32(0xDEAD_BEEF), [0x17, 0xF8, 0xE9, 0x1A]);
        assert_eq!(decode_u32([0x17, 0xF8, 0xE9, 0x1A]), 0xDEAD_BEEF);

        assert_eq!(encode_f32(1.1), [0x1B, 0x1B, 0x90, 0xB3]);
        assert_eq!(
            decode_f32([0x1B, 0x1B, 0x90, 0xB3]).to_bits(),
            1.1_f32.to_bits()
        );
        assert_eq!(encode_f32(350.0), [0xBA, 0x7C, 0x72, 0x69]);
        assert_eq!(
            decode_f32([0xBA, 0x7C, 0x72, 0x69]).to_bits(),
            350.0_f32.to_bits()
        );
    }

    #[test]
    fn preserves_the_csharp_modulo_255_boundary() {
        assert_eq!(encode_bytes(&[0]).unwrap(), [0xBA]);
        assert_eq!(encode_bytes(&[254]).unwrap(), [0x46]);
        assert_eq!(decode_bytes(&[0x46]).unwrap(), [254]);

        // This is deliberately not a round trip: the original encoder maps
        // 255 through table index zero because it uses `% 255`.
        assert_eq!(encode_bytes(&[255]).unwrap(), [0xBA]);
        assert_eq!(decode_bytes(&[0xBA]).unwrap(), [0]);
    }

    #[test]
    fn rejects_a_buffer_that_would_wrap_the_legacy_byte_counter() {
        let error = encode_bytes(&[0; 256]).unwrap_err();
        assert_eq!(error, EncodedScalarError::TooLong { length: 256 });
        assert_eq!(
            decode_bytes(&[0; 256]).unwrap_err(),
            EncodedScalarError::TooLong { length: 256 }
        );
    }
}
