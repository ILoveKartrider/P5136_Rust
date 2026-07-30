//! Transport-boundary packet diagnostics.
//!
//! Every datagram or logical TCP frame is recorded at the point where the
//! server accepts it or schedules it for socket output. The payload preview is
//! capped so one hostile maximum-sized frame cannot exhaust the diagnostic
//! disk.

use std::{
    fmt,
    net::SocketAddr,
    sync::{LazyLock, Mutex},
    time::Instant,
};

/// The largest payload prefix retained as hexadecimal in one packet record.
///
/// A record is still emitted for every packet above this limit, with its full
/// length and a `truncated = true` marker.  Normal P5136 packets are far below
/// this limit, while the bound prevents a single 1 MiB login frame from
/// becoming a 2 MiB log line.
pub(crate) const MAX_PACKET_LOG_BYTES: usize = 4 * 1_024;
const MAX_PACKET_LOG_RECORDS_PER_SECOND: usize = 512;

static PACKET_LOG_BUDGET: LazyLock<Mutex<PacketLogBudget>> =
    LazyLock::new(|| Mutex::new(PacketLogBudget::new(Instant::now())));

struct PacketLogBudget {
    window_started: Instant,
    records: usize,
    suppressed: usize,
}

impl PacketLogBudget {
    const fn new(window_started: Instant) -> Self {
        Self {
            window_started,
            records: 0,
            suppressed: 0,
        }
    }

    /// Returns whether a raw record can be emitted plus a prior-window loss
    /// count that should be reported before the next successful record.
    fn take_slot(&mut self, now: Instant) -> (bool, usize) {
        let mut prior_suppressed = 0;
        if now.duration_since(self.window_started).as_secs() >= 1 {
            self.window_started = now;
            self.records = 0;
            prior_suppressed = std::mem::take(&mut self.suppressed);
        }
        if self.records < MAX_PACKET_LOG_RECORDS_PER_SECOND {
            self.records += 1;
            (true, prior_suppressed)
        } else {
            self.suppressed = self.suppressed.saturating_add(1);
            (false, prior_suppressed)
        }
    }
}

#[derive(Clone, Copy)]
pub(crate) enum PacketDirection {
    Received,
    Sent,
}

impl PacketDirection {
    const fn label(self) -> &'static str {
        match self {
            Self::Received => "received",
            Self::Sent => "sent",
        }
    }
}

/// Emits one structured, raw-payload packet record.
///
/// `kind` distinguishes decoded logical TCP payloads from UDP wire datagrams.
/// The first four bytes are also included as a little-endian word when present
/// (the logical TCP/Messenger packet hash); short and malformed input is
/// intentionally logged rather than dropped by the logger.
pub(crate) fn trace_packet(
    transport: &'static str,
    kind: &'static str,
    direction: PacketDirection,
    peer: Option<SocketAddr>,
    bytes: &[u8],
) {
    let (permitted, prior_suppressed) = PACKET_LOG_BUDGET
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take_slot(Instant::now());
    if prior_suppressed != 0 {
        tracing::warn!(
            target: "p5136_packet",
            dropped_records = prior_suppressed,
            maximum_records_per_second = MAX_PACKET_LOG_RECORDS_PER_SECOND,
            "packet diagnostics were rate-limited"
        );
    }
    if !permitted {
        return;
    }
    let captured = bytes.len().min(MAX_PACKET_LOG_BYTES);
    let first_word_le = bytes
        .get(..4)
        .map(|header| u32::from_le_bytes([header[0], header[1], header[2], header[3]]));
    tracing::debug!(
        target: "p5136_packet",
        transport,
        kind,
        direction = direction.label(),
        peer = ?peer,
        bytes = bytes.len(),
        captured_bytes = captured,
        truncated = bytes.len() > captured,
        first_word_le = ?first_word_le.map(Hash),
        raw_hex = %HexPreview(&bytes[..captured]),
        "P5136 packet"
    );
}

struct Hash(u32);

impl fmt::Debug for Hash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "0x{:08X}", self.0)
    }
}

struct HexPreview<'a>(&'a [u8]);

impl fmt::Display for HexPreview<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut encoded = String::with_capacity(self.0.len() * 2);
        for byte in self.0 {
            encoded.push(char::from(HEX[usize::from(byte >> 4)]));
            encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
        }
        formatter.write_str(&encoded)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{HexPreview, MAX_PACKET_LOG_RECORDS_PER_SECOND, PacketLogBudget};

    #[test]
    fn hex_preview_uses_uppercase_compact_hex() {
        assert_eq!(HexPreview(&[0, 10, 255]).to_string(), "000AFF");
    }

    #[test]
    fn rate_budget_limits_one_window_and_reports_prior_drops() {
        let start = Instant::now();
        let mut budget = PacketLogBudget::new(start);
        for _ in 0..MAX_PACKET_LOG_RECORDS_PER_SECOND {
            assert_eq!(budget.take_slot(start), (true, 0));
        }
        assert_eq!(budget.take_slot(start), (false, 0));
        assert_eq!(budget.take_slot(start + Duration::from_secs(1)), (true, 1));
    }
}
