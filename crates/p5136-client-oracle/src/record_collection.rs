//! Independent reconstruction of the stock `PrStartCollectRecord` consumer.
//!
//! This module hard-codes the native packet hash and does not share the
//! production serializer. The recovered client reader consumes exactly one
//! raw byte after the packet hash. The common `GameStage` consumer passes the
//! logical inverse of that byte to `sub_AE6A00` and then stores the original
//! byte in its owned race state.

use crate::{DecodeError, cursor::Cursor};

const START_COLLECT_RECORD_REPLY_HASH: u32 = 0x52A4_07F5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartCollectRecordAction {
    /// Exact byte stored by the native consumer. The client accepts any byte,
    /// although the production writer emits only canonical 0/1 values.
    pub stored_flag: u8,
    /// Argument passed to native helper `sub_AE6A00` before `stored_flag` is
    /// committed. This is exactly `stored_flag == 0`.
    pub collector_gate_argument: bool,
}

impl StartCollectRecordAction {
    #[must_use]
    pub const fn flag_is_nonzero(self) -> bool {
        self.stored_flag != 0
    }
}

/// Decodes the exact five-byte reply and models the recovered consumer branch.
pub fn decode_start_collect_record_reply(
    packet: &[u8],
) -> Result<StartCollectRecordAction, DecodeError> {
    let mut reader = Cursor::new(packet);
    reader.expect_hash("PrStartCollectRecord", START_COLLECT_RECORD_REPLY_HASH)?;
    let stored_flag = reader.u8()?;
    reader.finish()?;
    Ok(StartCollectRecordAction {
        stored_flag,
        collector_gate_argument: stored_flag == 0,
    })
}
