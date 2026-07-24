use crate::{codec_error::PinCodecError, limits::CodecLimits};

pub(crate) struct WireReader<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> WireReader<'a> {
    pub(crate) const fn new(input: &'a [u8]) -> Self {
        Self { input, offset: 0 }
    }

    pub(crate) fn read_u8(&mut self) -> Result<u8, PinCodecError> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn read_bool(&mut self) -> Result<bool, PinCodecError> {
        // The original InPacket treats only byte 1 as true and every other
        // value as false.
        Ok(self.read_u8()? == 1)
    }

    pub(crate) fn read_u16(&mut self) -> Result<u16, PinCodecError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub(crate) fn read_u32(&mut self) -> Result<u32, PinCodecError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn read_i32(&mut self) -> Result<i32, PinCodecError> {
        let bytes = self.take(4)?;
        Ok(i32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub(crate) fn read_count(
        &mut self,
        field: &'static str,
        maximum: usize,
    ) -> Result<usize, PinCodecError> {
        let signed = self.read_i32()?;
        let count = usize::try_from(signed).map_err(|_| PinCodecError::NegativeLength {
            field,
            value: signed,
        })?;
        enforce_limit(field, count, maximum)?;
        Ok(count)
    }

    pub(crate) fn read_string(&mut self, limits: &CodecLimits) -> Result<String, PinCodecError> {
        let unit_count = self.read_count("UTF-16 string", limits.max_string_code_units)?;
        let byte_count = unit_count
            .checked_mul(2)
            .ok_or(PinCodecError::LengthOverflow("UTF-16 string"))?;
        let bytes = self.take(byte_count)?;

        let mut units = Vec::new();
        units
            .try_reserve_exact(unit_count)
            .map_err(|_| PinCodecError::Allocation("UTF-16 string"))?;
        units.extend(
            bytes
                .chunks_exact(2)
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]])),
        );
        // Encoding.Unicode in the reference implementation uses replacement
        // fallback for malformed surrogate pairs.
        Ok(String::from_utf16_lossy(&units))
    }

    pub(crate) const fn remaining(&self) -> &'a [u8] {
        // offset only advances through take(), which bounds-checks it.
        self.input.split_at(self.offset).1
    }

    pub(crate) fn take(&mut self, length: usize) -> Result<&'a [u8], PinCodecError> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or(PinCodecError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        let bytes = self
            .input
            .get(self.offset..end)
            .ok_or(PinCodecError::Truncated {
                offset: self.offset,
                needed: length,
            })?;
        self.offset = end;
        Ok(bytes)
    }
}

pub(crate) struct WireWriter {
    output: Vec<u8>,
    maximum: usize,
}

impl WireWriter {
    pub(crate) const fn new(maximum: usize) -> Self {
        Self {
            output: Vec::new(),
            maximum,
        }
    }

    pub(crate) fn write_u8(&mut self, value: u8) -> Result<(), PinCodecError> {
        self.reserve(1, "PIN payload")?;
        self.output.push(value);
        Ok(())
    }

    pub(crate) fn write_bool(&mut self, value: bool) -> Result<(), PinCodecError> {
        self.write_u8(u8::from(value))
    }

    pub(crate) fn write_u16(&mut self, value: u16) -> Result<(), PinCodecError> {
        self.write_bytes(&value.to_le_bytes())
    }

    pub(crate) fn write_u32(&mut self, value: u32) -> Result<(), PinCodecError> {
        self.write_bytes(&value.to_le_bytes())
    }

    pub(crate) fn write_i32(&mut self, value: i32) -> Result<(), PinCodecError> {
        self.write_bytes(&value.to_le_bytes())
    }

    pub(crate) fn write_count(
        &mut self,
        value: usize,
        field: &'static str,
        maximum: usize,
    ) -> Result<(), PinCodecError> {
        enforce_limit(field, value, maximum)?;
        let value = i32::try_from(value).map_err(|_| PinCodecError::LengthOverflow(field))?;
        self.write_i32(value)
    }

    pub(crate) fn write_string(
        &mut self,
        value: &str,
        limits: &CodecLimits,
    ) -> Result<(), PinCodecError> {
        let unit_count = value.encode_utf16().count();
        enforce_limit("UTF-16 string", unit_count, limits.max_string_code_units)?;
        self.write_count(unit_count, "UTF-16 string", limits.max_string_code_units)?;
        let byte_count = unit_count
            .checked_mul(2)
            .ok_or(PinCodecError::LengthOverflow("UTF-16 string"))?;
        self.reserve(byte_count, "PIN payload")?;
        for unit in value.encode_utf16() {
            self.output.extend_from_slice(&unit.to_le_bytes());
        }
        Ok(())
    }

    pub(crate) fn write_bytes(&mut self, bytes: &[u8]) -> Result<(), PinCodecError> {
        self.reserve(bytes.len(), "PIN payload")?;
        self.output.extend_from_slice(bytes);
        Ok(())
    }

    pub(crate) fn into_inner(self) -> Vec<u8> {
        self.output
    }

    fn reserve(&mut self, additional: usize, kind: &'static str) -> Result<(), PinCodecError> {
        let new_length = self
            .output
            .len()
            .checked_add(additional)
            .ok_or(PinCodecError::LengthOverflow(kind))?;
        enforce_limit(kind, new_length, self.maximum)?;
        self.output
            .try_reserve_exact(additional)
            .map_err(|_| PinCodecError::Allocation(kind))
    }
}

pub(crate) fn enforce_limit(
    kind: &'static str,
    actual: usize,
    maximum: usize,
) -> Result<(), PinCodecError> {
    if actual > maximum {
        Err(PinCodecError::LimitExceeded {
            kind,
            actual,
            maximum,
        })
    } else {
        Ok(())
    }
}

pub(crate) fn reserve_items<T>(
    values: &mut Vec<T>,
    count: usize,
    kind: &'static str,
) -> Result<(), PinCodecError> {
    values
        .try_reserve_exact(count)
        .map_err(|_| PinCodecError::Allocation(kind))
}
