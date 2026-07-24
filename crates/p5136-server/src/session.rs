use std::{io, net::SocketAddr};

use p5136_core::{
    frame::{self, FrameError},
    handshake,
    packet::PacketError,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time,
};

use crate::{ServerConfig, WorldError, WorldHandle};

#[derive(Debug, Error)]
pub enum LoginSessionError {
    #[error("login socket I/O failed")]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error(transparent)]
    World(#[from] WorldError),

    #[error("client did not send its first encrypted packet before the login timeout")]
    LoginTimeout,

    #[error("logical login packet is shorter than its four-byte name hash")]
    MissingPacketHash,
}

/// Reads exactly one encrypted frame from an arbitrary async byte stream.
///
/// The encoded length is validated before the body allocation. `read_exact`
/// makes the function insensitive to TCP fragmentation and coalescing.
pub async fn read_encrypted_frame<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    reader.read_exact(&mut header).await?;
    let encoded_header = u32::from_le_bytes(header);
    let body_length = frame::encrypted_body_length(encoded_header, *iv, maximum)?;

    let mut wire = Vec::with_capacity(body_length + 4);
    wire.extend_from_slice(&header);
    wire.resize(body_length + 4, 0);
    reader.read_exact(&mut wire[4..]).await?;
    Ok(frame::decode_encrypted(&wire, iv, maximum)?)
}

pub(crate) async fn run_login_session(
    mut stream: TcpStream,
    peer: SocketAddr,
    config: ServerConfig,
    world: WorldHandle,
) -> Result<(), LoginSessionError> {
    let session_id = world.register_session(peer).await?;
    let registration = SessionRegistration {
        id: session_id,
        world: world.clone(),
        closed: false,
    };
    let result = run_registered_session(&mut stream, &config).await;
    let close_result = registration.close().await;

    match (result, close_result) {
        (Err(error), _) => Err(error),
        (Ok(()), Err(error)) => Err(error.into()),
        (Ok(()), Ok(())) => Ok(()),
    }
}

struct SessionRegistration {
    id: crate::SessionId,
    world: WorldHandle,
    closed: bool,
}

impl SessionRegistration {
    async fn close(mut self) -> Result<(), WorldError> {
        self.world.session_closed(self.id).await?;
        self.closed = true;
        Ok(())
    }
}

impl Drop for SessionRegistration {
    fn drop(&mut self) {
        if !self.closed {
            self.world.try_session_closed(self.id);
        }
    }
}

async fn run_registered_session(
    stream: &mut TcpStream,
    config: &ServerConfig,
) -> Result<(), LoginSessionError> {
    time::sleep(config.first_message_delay).await;

    // Install the receive state before putting the server-first frame on the
    // wire. No client read begins before this point.
    let mut receive_iv = handshake::initial_iv();
    let payload = handshake::first_message_payload()?;
    let wire = frame::encode_plain(&payload, config.max_login_payload)?;
    stream.write_all(&wire).await?;

    let first = time::timeout(
        config.login_timeout,
        read_encrypted_frame(stream, &mut receive_iv, config.max_login_payload),
    )
    .await
    .map_err(|_| LoginSessionError::LoginTimeout)??;
    trace_packet(peer_label(stream), &first)?;

    loop {
        let packet =
            read_encrypted_frame(stream, &mut receive_iv, config.max_login_payload).await?;
        trace_packet(peer_label(stream), &packet)?;
    }
}

fn peer_label(stream: &TcpStream) -> Option<SocketAddr> {
    stream.peer_addr().ok()
}

fn trace_packet(peer: Option<SocketAddr>, packet: &[u8]) -> Result<(), LoginSessionError> {
    let hash_bytes = packet
        .get(..4)
        .ok_or(LoginSessionError::MissingPacketHash)?;
    let hash = u32::from_le_bytes([hash_bytes[0], hash_bytes[1], hash_bytes[2], hash_bytes[3]]);
    tracing::debug!(
        ?peer,
        packet_hash = format_args!("0x{hash:08X}"),
        "login packet"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use p5136_core::frame::{DEFAULT_MAX_PAYLOAD, encode_encrypted};
    use tokio::io::{AsyncWriteExt, duplex};

    use super::read_encrypted_frame;

    #[tokio::test]
    async fn fragmented_and_coalesced_frames_decode_in_order() {
        let (mut writer, mut reader) = duplex(4_096);
        let mut send_iv = 0xa1b7_1c9b;
        let first = encode_encrypted(b"first-packet", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        let second = encode_encrypted(b"second-packet", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();

        let write_task = tokio::spawn(async move {
            for byte in &first[..7] {
                writer.write_all(&[*byte]).await.unwrap();
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
            writer.write_all(&first[7..]).await.unwrap();
            writer.write_all(&second).await.unwrap();
        });

        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
                .await
                .unwrap(),
            b"first-packet"
        );
        assert_eq!(
            read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
                .await
                .unwrap(),
            b"second-packet"
        );
        write_task.await.unwrap();
        assert_eq!(receive_iv, send_iv);
    }
}
