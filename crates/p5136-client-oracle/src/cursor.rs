use std::{error::Error, fmt};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    UnexpectedEof {
        offset: usize,
        needed: usize,
        remaining: usize,
    },
    UnexpectedHash {
        packet: &'static str,
        expected: u32,
        actual: u32,
    },
    InvalidCount {
        field: &'static str,
        value: i32,
        maximum: usize,
    },
    InvalidUtf16 {
        field: &'static str,
    },
    UnsupportedDiscriminant {
        field: &'static str,
        value: i32,
    },
    TrailingBytes {
        offset: usize,
        remaining: usize,
    },
    InvalidSequence {
        expected: &'static str,
        actual: &'static str,
    },
}

impl fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl Error for DecodeError {}

pub(crate) struct Cursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    pub(crate) const fn position(&self) -> usize {
        self.offset
    }

    pub(crate) fn expect_hash(
        &mut self,
        packet: &'static str,
        expected: u32,
    ) -> Result<(), DecodeError> {
        let actual = self.u32()?;
        if actual == expected {
            Ok(())
        } else {
            Err(DecodeError::UnexpectedHash {
                packet,
                expected,
                actual,
            })
        }
    }

    pub(crate) fn u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.array::<1>()?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, DecodeError> {
        Ok(u16::from_le_bytes(self.array()?))
    }

    pub(crate) fn i16(&mut self) -> Result<i16, DecodeError> {
        Ok(i16::from_le_bytes(self.array()?))
    }

    pub(crate) fn u32(&mut self) -> Result<u32, DecodeError> {
        Ok(u32::from_le_bytes(self.array()?))
    }

    pub(crate) fn i32(&mut self) -> Result<i32, DecodeError> {
        Ok(i32::from_le_bytes(self.array()?))
    }

    pub(crate) fn count(
        &mut self,
        field: &'static str,
        maximum: usize,
    ) -> Result<usize, DecodeError> {
        let value = self.i32()?;
        let Ok(value) = usize::try_from(value) else {
            return Err(DecodeError::InvalidCount {
                field,
                value,
                maximum,
            });
        };
        if value > maximum {
            return Err(DecodeError::InvalidCount {
                field,
                value: i32::try_from(value).unwrap_or(i32::MAX),
                maximum,
            });
        }
        Ok(value)
    }

    pub(crate) fn utf16(
        &mut self,
        field: &'static str,
        maximum: usize,
    ) -> Result<String, DecodeError> {
        let count = self.count(field, maximum)?;
        let mut units = Vec::with_capacity(count);
        for _ in 0..count {
            units.push(self.u16()?);
        }
        String::from_utf16(&units).map_err(|_| DecodeError::InvalidUtf16 { field })
    }

    pub(crate) fn array<const LENGTH: usize>(&mut self) -> Result<[u8; LENGTH], DecodeError> {
        let bytes = self.bytes(LENGTH)?;
        let mut output = [0; LENGTH];
        output.copy_from_slice(bytes);
        Ok(output)
    }

    pub(crate) fn bytes(&mut self, length: usize) -> Result<&'a [u8], DecodeError> {
        let Some(end) = self.offset.checked_add(length) else {
            return Err(DecodeError::UnexpectedEof {
                offset: self.offset,
                needed: length,
                remaining: self.bytes.len().saturating_sub(self.offset),
            });
        };
        let Some(bytes) = self.bytes.get(self.offset..end) else {
            return Err(DecodeError::UnexpectedEof {
                offset: self.offset,
                needed: length,
                remaining: self.bytes.len().saturating_sub(self.offset),
            });
        };
        self.offset = end;
        Ok(bytes)
    }

    pub(crate) fn finish(self) -> Result<(), DecodeError> {
        let remaining = self.bytes.len().saturating_sub(self.offset);
        if remaining == 0 {
            Ok(())
        } else {
            Err(DecodeError::TrailingBytes {
                offset: self.offset,
                remaining,
            })
        }
    }
}
