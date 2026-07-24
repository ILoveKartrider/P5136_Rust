use std::io::{Read, Write};

use flate2::{Compression, read::ZlibDecoder, write::ZlibEncoder};
use p5136_core::adler32;
use thiserror::Error;

use crate::limits::CodecLimits;

pub const FLAG_ZLIB: u8 = 0x01;
pub const FLAG_KART_CRYPTO: u8 = 0x02;
pub const DEFAULT_KART_CRYPTO_KEY: u32 = 0x3369_9633;

const ENCODED_MAGIC: u8 = b'S';
const KART_CRYPTO_SEED_XOR: u32 = 0x8473_FBC1;
const KART_CRYPTO_STEP: u32 = 0x7B8C_043F;
const KART_CRYPTO_BLOCK_SIZE: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockEncoding {
    pub flags: u8,
    pub kart_crypto_key: Option<u32>,
}

impl Default for BlockEncoding {
    fn default() -> Self {
        Self {
            flags: FLAG_ZLIB | FLAG_KART_CRYPTO,
            kart_crypto_key: Some(DEFAULT_KART_CRYPTO_KEY),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecodedBlock {
    pub bytes: Vec<u8>,
    pub encoding: BlockEncoding,
}

#[derive(Debug, Error)]
pub enum EncodedBlockError {
    #[error("{kind} size {actual} exceeds configured maximum {maximum}")]
    LimitExceeded {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("encoded block ended at byte {offset}; needed {needed} more bytes")]
    Truncated { offset: usize, needed: usize },

    #[error("encoded block checksum mismatch: expected 0x{expected:08X}, got 0x{actual:08X}")]
    ChecksumMismatch { expected: u32, actual: u32 },

    #[error("zlib decoded length {actual} does not match declared length {expected}")]
    DecodedLengthMismatch { expected: usize, actual: usize },

    #[error("invalid zlib payload")]
    Zlib(#[source] std::io::Error),

    #[error("encoded block is too large for its u32 length field")]
    LengthOverflow,

    #[error("failed to reserve memory for {0}")]
    Allocation(&'static str),
}

pub fn decode(input: &[u8], limits: &CodecLimits) -> Result<DecodedBlock, EncodedBlockError> {
    enforce_limit("encoded block", input.len(), limits.max_encoded_block_bytes)?;

    if input.first() != Some(&ENCODED_MAGIC) {
        enforce_limit("decoded block", input.len(), limits.max_decoded_block_bytes)?;
        return Ok(DecodedBlock {
            bytes: copy_bounded(input, "raw decoded block")?,
            // PINFile initializes these defaults before it discovers that a
            // legacy input is not wrapped in an S block.
            encoding: BlockEncoding::default(),
        });
    }

    let mut reader = BlockReader::new(input);
    let magic = reader.read_u8()?;
    debug_assert_eq!(magic, ENCODED_MAGIC);
    let flags = reader.read_u8()?;
    let expected_checksum = reader.read_u32()?;
    let kart_crypto_key = if flags & FLAG_KART_CRYPTO != 0 {
        Some(reader.read_u32()?)
    } else {
        None
    };
    let declared_length = if flags & FLAG_ZLIB != 0 {
        let length =
            usize::try_from(reader.read_u32()?).map_err(|_| EncodedBlockError::LengthOverflow)?;
        enforce_limit(
            "declared decoded block",
            length,
            limits.max_decoded_block_bytes,
        )?;
        Some(length)
    } else {
        None
    };

    let mut body = copy_bounded(reader.remaining(), "encoded block body")?;
    if let Some(key) = kart_crypto_key {
        apply_kart_crypto(&mut body, key);
    }

    let decoded = if let Some(expected_length) = declared_length {
        decode_zlib_bounded(&body, expected_length, limits)?
    } else {
        enforce_limit("decoded block", body.len(), limits.max_decoded_block_bytes)?;
        body
    };

    let actual_checksum = adler32::hash(&decoded, 0);
    if actual_checksum != expected_checksum {
        return Err(EncodedBlockError::ChecksumMismatch {
            expected: expected_checksum,
            actual: actual_checksum,
        });
    }

    Ok(DecodedBlock {
        bytes: decoded,
        encoding: BlockEncoding {
            flags,
            kart_crypto_key,
        },
    })
}

pub fn encode(
    input: &[u8],
    encoding: BlockEncoding,
    limits: &CodecLimits,
) -> Result<Vec<u8>, EncodedBlockError> {
    enforce_limit("decoded block", input.len(), limits.max_decoded_block_bytes)?;
    let checksum = adler32::hash(input, 0);

    let mut body = if encoding.flags & FLAG_ZLIB != 0 {
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(input).map_err(EncodedBlockError::Zlib)?;
        encoder.finish().map_err(EncodedBlockError::Zlib)?
    } else {
        copy_bounded(input, "encoded block body")?
    };

    let key = if encoding.flags & FLAG_KART_CRYPTO != 0 {
        let key = encoding.kart_crypto_key.unwrap_or(checksum);
        apply_kart_crypto(&mut body, key);
        Some(key)
    } else {
        None
    };

    let header_length =
        6 + usize::from(key.is_some()) * 4 + usize::from(encoding.flags & FLAG_ZLIB != 0) * 4;
    let total_length = header_length
        .checked_add(body.len())
        .ok_or(EncodedBlockError::LengthOverflow)?;
    enforce_limit(
        "encoded block",
        total_length,
        limits.max_encoded_block_bytes,
    )?;

    let mut output = Vec::new();
    output
        .try_reserve_exact(total_length)
        .map_err(|_| EncodedBlockError::Allocation("encoded block"))?;
    output.push(ENCODED_MAGIC);
    output.push(encoding.flags);
    output.extend_from_slice(&checksum.to_le_bytes());
    if let Some(key) = key {
        output.extend_from_slice(&key.to_le_bytes());
    }
    if encoding.flags & FLAG_ZLIB != 0 {
        let length = u32::try_from(input.len()).map_err(|_| EncodedBlockError::LengthOverflow)?;
        output.extend_from_slice(&length.to_le_bytes());
    }
    output.extend_from_slice(&body);
    Ok(output)
}

fn decode_zlib_bounded(
    body: &[u8],
    expected_length: usize,
    limits: &CodecLimits,
) -> Result<Vec<u8>, EncodedBlockError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(expected_length)
        .map_err(|_| EncodedBlockError::Allocation("decoded zlib block"))?;

    let read_limit = u64::try_from(limits.max_decoded_block_bytes)
        .unwrap_or(u64::MAX)
        .saturating_add(1);
    let decoder = ZlibDecoder::new(body);
    decoder
        .take(read_limit)
        .read_to_end(&mut output)
        .map_err(EncodedBlockError::Zlib)?;

    enforce_limit(
        "decoded zlib block",
        output.len(),
        limits.max_decoded_block_bytes,
    )?;
    if output.len() != expected_length {
        return Err(EncodedBlockError::DecodedLengthMismatch {
            expected: expected_length,
            actual: output.len(),
        });
    }
    Ok(output)
}

fn apply_kart_crypto(bytes: &mut [u8], key: u32) {
    let mut stream = [0_u8; KART_CRYPTO_BLOCK_SIZE];
    let mut word = key ^ KART_CRYPTO_SEED_XOR;
    for chunk in stream.chunks_exact_mut(4) {
        chunk.copy_from_slice(&word.to_le_bytes());
        word = word.wrapping_sub(KART_CRYPTO_STEP);
    }

    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte ^= stream[index % KART_CRYPTO_BLOCK_SIZE];
    }
}

fn copy_bounded(input: &[u8], kind: &'static str) -> Result<Vec<u8>, EncodedBlockError> {
    let mut output = Vec::new();
    output
        .try_reserve_exact(input.len())
        .map_err(|_| EncodedBlockError::Allocation(kind))?;
    output.extend_from_slice(input);
    Ok(output)
}

fn enforce_limit(
    kind: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), EncodedBlockError> {
    if actual > maximum {
        Err(EncodedBlockError::LimitExceeded {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

struct BlockReader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> BlockReader<'a> {
    const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    fn read_u8(&mut self) -> Result<u8, EncodedBlockError> {
        Ok(self.take(1)?[0])
    }

    fn read_u32(&mut self) -> Result<u32, EncodedBlockError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    const fn remaining(&self) -> &'a [u8] {
        // offset can only advance through take(), which bounds-checks it.
        self.input.split_at(self.offset).1
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], EncodedBlockError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(EncodedBlockError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(EncodedBlockError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        self.offset = end;
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        BlockEncoding, DEFAULT_KART_CRYPTO_KEY, FLAG_KART_CRYPTO, FLAG_ZLIB, decode, encode,
    };
    use crate::limits::CodecLimits;

    #[test]
    fn roundtrips_each_known_encoding_combination() {
        let input = b"synthetic P5136 encoded block payload".repeat(20);
        for flags in 0..=FLAG_ZLIB | FLAG_KART_CRYPTO {
            let encoding = BlockEncoding {
                flags,
                kart_crypto_key: (flags & FLAG_KART_CRYPTO != 0).then_some(DEFAULT_KART_CRYPTO_KEY),
            };
            let encoded = encode(&input, encoding, &CodecLimits::default()).unwrap();
            let decoded = decode(&encoded, &CodecLimits::default()).unwrap();
            assert_eq!(decoded.bytes, input);
            assert_eq!(decoded.encoding, encoding);
        }
    }

    #[test]
    fn rejects_a_compressed_payload_above_the_decoded_limit() {
        let generous = CodecLimits::default();
        let encoded = encode(&[0x41; 1024], BlockEncoding::default(), &generous).unwrap();
        let tight = CodecLimits {
            max_decoded_block_bytes: 64,
            ..generous
        };
        assert!(decode(&encoded, &tight).is_err());
    }
}
