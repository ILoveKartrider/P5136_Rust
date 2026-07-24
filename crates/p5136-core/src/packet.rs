//! Little-endian packet readers and writers.

use std::string::FromUtf16Error;

use thiserror::Error;

use crate::adler32;

#[derive(Debug, Error)]
pub enum PacketError {
    #[error("packet ended at byte {offset}; needed {needed} more bytes")]
    Truncated { offset: usize, needed: usize },

    #[error("negative UTF-16 code-unit length {0}")]
    NegativeStringLength(i32),

    #[error("UTF-16 string is too long for the P5136 i32 length field")]
    StringTooLong,

    #[error("UTF-16 length overflows the host address space")]
    StringLengthOverflow,

    #[error("invalid UTF-16 string")]
    InvalidUtf16(#[from] FromUtf16Error),
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct PacketWriter {
    bytes: Vec<u8>,
}

impl PacketWriter {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a packet with the zero-seeded Adler-32 hash of its RTTI name.
    #[must_use]
    pub fn named(name: &str) -> Self {
        let mut writer = Self::new();
        writer.write_u32(adler32::packet_hash(name));
        writer
    }

    pub fn write_u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub fn write_u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_i32(&mut self, value: i32) {
        self.bytes.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_bytes(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    /// Writes a .NET-compatible UTF-16LE string: i32 code-unit count followed
    /// by exactly that many code units, with no trailing NUL.
    pub fn write_utf16(&mut self, value: &str) -> Result<(), PacketError> {
        let units: Vec<u16> = value.encode_utf16().collect();
        let length = i32::try_from(units.len()).map_err(|_| PacketError::StringTooLong)?;
        self.write_i32(length);
        for unit in units {
            self.write_u16(unit);
        }
        Ok(())
    }

    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    #[must_use]
    pub fn into_inner(self) -> Vec<u8> {
        self.bytes
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PacketReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> PacketReader<'a> {
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub fn read_u8(&mut self) -> Result<u8, PacketError> {
        Ok(self.take(1)?[0])
    }

    pub fn read_u16(&mut self) -> Result<u16, PacketError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_u32(&mut self) -> Result<u32, PacketError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_i32(&mut self) -> Result<i32, PacketError> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_utf16(&mut self) -> Result<String, PacketError> {
        let signed_length = self.read_i32()?;
        let length = usize::try_from(signed_length)
            .map_err(|_| PacketError::NegativeStringLength(signed_length))?;
        let byte_length = length
            .checked_mul(2)
            .ok_or(PacketError::StringLengthOverflow)?;
        let bytes = self.take(byte_length)?;
        let units = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        Ok(String::from_utf16(&units)?)
    }

    #[must_use]
    pub fn remaining(&self) -> &'a [u8] {
        &self.bytes[self.offset..]
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], PacketError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(PacketError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        let value = self
            .bytes
            .get(self.offset..end)
            .ok_or(PacketError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        self.offset = end;
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::{PacketReader, PacketWriter};

    #[test]
    fn unicode_length_counts_utf16_code_units_without_a_terminator() {
        let mut writer = PacketWriter::new();
        writer.write_utf16("A🏎").unwrap();

        assert_eq!(
            writer.as_slice(),
            &[3, 0, 0, 0, 0x41, 0, 0x3c, 0xd8, 0xce, 0xdf]
        );

        let mut reader = PacketReader::new(writer.as_slice());
        assert_eq!(reader.read_utf16().unwrap(), "A🏎");
        assert!(reader.remaining().is_empty());
    }
}
