use thiserror::Error;

use crate::encoded_block::EncodedBlockError;

#[derive(Debug, Error)]
pub enum PinCodecError {
    #[error("{kind} size/count {actual} exceeds configured maximum {maximum}")]
    LimitExceeded {
        kind: &'static str,
        actual: usize,
        maximum: usize,
    },

    #[error("{field} has negative length/count {value}")]
    NegativeLength { field: &'static str, value: i32 },

    #[error("PIN payload ended at byte {offset}; needed {needed} more bytes")]
    Truncated { offset: usize, needed: usize },

    #[error("failed to reserve memory for {0}")]
    Allocation(&'static str),

    #[error("{0} is too large for the P5136 i32 length field")]
    LengthOverflow(&'static str),

    #[error("invalid PIN object magic 0x{actual:08X}; expected 0x{expected:08X}")]
    InvalidPinMagic { expected: u32, actual: u32 },

    #[error("PIN protocol {actual} is not P5136 ({expected})")]
    WrongProtocol { expected: u16, actual: u16 },

    #[error("P5136 PIN contains no authentication methods")]
    MissingAuthenticationMethods,

    #[error("patched P5136 PIN failed endpoint verification")]
    EndpointVerificationFailed,

    #[error("patched P5136 PIN failed storage-path verification")]
    StorageVerificationFailed,

    #[error("BML node contains duplicate attribute {0:?}")]
    DuplicateBmlAttribute(String),

    #[error("encoded block error")]
    EncodedBlock(#[from] EncodedBlockError),
}
