use std::{io, net::SocketAddr, time::Instant};

use chrono::{Local, NaiveDate, Timelike};
use p5136_core::{
    adler32,
    channel::{
        ChannelError, parse_pq_channel_movein, parse_pq_channel_switch, resolve_channel_id,
        serialize_pr_channel_move_in, serialize_pr_channel_switch,
    },
    frame::{self, FrameError},
    handshake,
    login::{
        LegacyTime, LoginError, PrLoginFields, parse_pq_login, serialize_pr_cn_authen_login,
        serialize_pr_login,
    },
    packet::PacketError,
};
use rand::Rng;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::oneshot,
    time,
};

use crate::{
    ChannelBinding, MigrationToken, ServerConfig, SessionId, UserNo, WorldError, WorldHandle,
};

#[derive(Debug, Error)]
pub enum LoginSessionError {
    #[error("login socket I/O failed")]
    Io(#[from] io::Error),

    #[error(transparent)]
    Frame(#[from] FrameError),

    #[error(transparent)]
    Packet(#[from] PacketError),

    #[error(transparent)]
    LoginProtocol(#[from] LoginError),

    #[error(transparent)]
    ChannelProtocol(#[from] ChannelError),

    #[error(transparent)]
    World(#[from] WorldError),

    #[error("client did not send its first encrypted packet before the login timeout")]
    LoginTimeout,

    #[error("logical login packet is shorter than its four-byte name hash")]
    MissingPacketHash,

    #[error(
        "P5136 static channel catalog has no record for game type {game_type} and preferred channel {preferred_channel}"
    )]
    UnsupportedChannel {
        game_type: u8,
        preferred_channel: u16,
    },

    #[error("PqChannelMovein contains invalid zero user number")]
    InvalidUserNo,

    #[error("PqChannelMovein contains invalid zero migration token")]
    InvalidMigrationToken,

    #[error("login session was superseded by a newer channel generation")]
    Superseded,
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
    let (session_id, mut cancellation) = world.register_login_session(peer).await?;
    let registration = SessionRegistration {
        id: session_id,
        world: world.clone(),
        closed: false,
    };
    let result =
        run_registered_session(&mut stream, &config, &world, session_id, &mut cancellation).await;
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
    world: &WorldHandle,
    session_id: SessionId,
    cancellation: &mut oneshot::Receiver<()>,
) -> Result<(), LoginSessionError> {
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        () = time::sleep(config.first_message_delay) => {}
    }

    // Install the receive state before putting the server-first frame on the
    // wire. No client read begins before this point.
    let mut receive_iv = handshake::initial_iv();
    let mut send_iv = handshake::initial_iv();
    let payload = handshake::first_message_payload()?;
    let wire = frame::encode_plain(&payload, config.max_login_payload)?;
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        result = stream.write_all(&wire) => result?,
    }

    let first = tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        result = time::timeout(
            config.login_timeout,
            read_encrypted_frame(stream, &mut receive_iv, config.max_login_payload),
        ) => result.map_err(|_| LoginSessionError::LoginTimeout)??,
    };
    trace_packet(peer_label(stream), &first)?;
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        result = process_and_write(stream, config, world, session_id, &first, &mut send_iv) => result?,
    }

    loop {
        let packet = tokio::select! {
            biased;
            _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
            result = read_encrypted_frame(stream, &mut receive_iv, config.max_login_payload) => result?,
        };
        trace_packet(peer_label(stream), &packet)?;
        tokio::select! {
            biased;
            _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
            result = process_and_write(stream, config, world, session_id, &packet, &mut send_iv) => result?,
        }
    }
}

async fn process_and_write(
    stream: &mut TcpStream,
    config: &ServerConfig,
    world: &WorldHandle,
    session_id: SessionId,
    packet: &[u8],
    send_iv: &mut u32,
) -> Result<(), LoginSessionError> {
    let responses = dispatch_packet(config, world, session_id, packet).await?;
    for response in responses {
        let wire = frame::encode_encrypted(&response, send_iv, config.max_login_payload)?;
        stream.write_all(&wire).await?;
    }
    Ok(())
}

async fn dispatch_packet(
    config: &ServerConfig,
    world: &WorldHandle,
    session_id: SessionId,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let hash = packet_hash(packet)?;
    if hash == adler32::packet_hash("PqCnAuthenLogin") {
        return Ok(vec![serialize_pr_cn_authen_login()?]);
    }

    if hash == adler32::packet_hash("PqLogin") {
        let login = parse_pq_login(packet)?;
        let identity = world.claim_identity(session_id, login.nickname).await?;
        return Ok(vec![serialize_pr_login(&PrLoginFields {
            time: current_legacy_time(),
            user_no: identity.user_no.get(),
            nickname: identity.nickname,
            pmap: 0,
            advertised_address: config.advertised_address,
            game_udp_port: config.ports.game_udp(),
            p2p_udp_port: config.ports.p2p_udp(),
            screen: 0,
        })?]);
    }

    if hash == adler32::packet_hash("PqChannelSwitch") {
        let request = parse_pq_channel_switch(packet)?;
        let selected_channel =
            resolve_channel_id(request.requested_game_type, request.preferred_channel_id).ok_or(
                LoginSessionError::UnsupportedChannel {
                    game_type: request.requested_game_type,
                    preferred_channel: request.preferred_channel_id,
                },
            )?;
        let token = random_migration_token();
        let permit = world
            .begin_migration(
                session_id,
                ChannelBinding {
                    channel_id: selected_channel,
                    game_type: request.requested_game_type,
                },
                token,
                Instant::now(),
            )
            .await?;
        return Ok(vec![serialize_pr_channel_switch(
            selected_channel,
            permit.token.get(),
            config.advertised_address,
            config.ports.login_tcp(),
        )]);
    }

    if hash == adler32::packet_hash("PqChannelMovein") {
        let request = parse_pq_channel_movein(packet)?;
        let user_no = UserNo::new(request.user_no).ok_or(LoginSessionError::InvalidUserNo)?;
        let token = MigrationToken::new(request.migration_token)
            .ok_or(LoginSessionError::InvalidMigrationToken)?;
        world
            .complete_migration(
                session_id,
                user_no,
                request.channel_id,
                token,
                Instant::now(),
            )
            .await?;
        return Ok(vec![serialize_pr_channel_move_in(
            config.ports.game_udp(),
            config.ports.p2p_udp(),
        )]);
    }

    // Identity-bound packets cannot be processed by a stale connection. Their
    // concrete handlers are ported incrementally on top of this fence.
    let _ = world.authorize_identity(session_id).await?;
    Ok(Vec::new())
}

fn packet_hash(packet: &[u8]) -> Result<u32, LoginSessionError> {
    let bytes = packet
        .get(..4)
        .ok_or(LoginSessionError::MissingPacketHash)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn random_migration_token() -> MigrationToken {
    let mut random = rand::rng();
    loop {
        if let Some(token) = MigrationToken::new(random.random()) {
            return token;
        }
    }
}

fn current_legacy_time() -> LegacyTime {
    let now = Local::now();
    let epoch = NaiveDate::from_ymd_opt(1900, 1, 1).expect("1900-01-01 is a valid date");
    let days = (now.date_naive() - epoch).num_days().rem_euclid(65_536);
    let quarter_seconds = now.num_seconds_from_midnight() / 4;
    LegacyTime {
        days_since_1900: u16::try_from(days).expect("modulo 65536 fits in u16"),
        quarter_seconds: u16::try_from(quarter_seconds)
            .expect("one day of quarter-seconds fits in u16"),
    }
}

fn peer_label(stream: &TcpStream) -> Option<SocketAddr> {
    stream.peer_addr().ok()
}

fn trace_packet(peer: Option<SocketAddr>, packet: &[u8]) -> Result<(), LoginSessionError> {
    let hash = packet_hash(packet)?;
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
