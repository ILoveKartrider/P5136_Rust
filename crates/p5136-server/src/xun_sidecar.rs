use std::{
    collections::HashMap,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex, MutexGuard},
    time::Duration,
};

use p5136_core::{
    dataraw_manifest::{
        DATARAW_PREFLIGHT_FRAME_LENGTH, DATARAW_PREFLIGHT_REQUEST_MAGIC, DataRawManifest,
        DataRawPreflightStatus, decode_dataraw_request, encode_dataraw_response,
    },
    nickname::{canonical_nickname_key, normalize_nickname},
    xun_sidecar_protocol::{
        XUN_PROFILE_FLAG_REMAINING_CONSUMERS, XUN_PROFILE_FLAG_SPEED_BOOST_GAUGE,
        XUN_SIDECAR_CLIENT_EVENT_HEADER_LENGTH, XUN_SIDECAR_CLIENT_EVENT_MAGIC,
        XUN_SIDECAR_CLIENT_EVENT_RACE_RESET, XUN_SIDECAR_HANDSHAKE_HEADER_LENGTH,
        XUN_SIDECAR_HANDSHAKE_MAGIC, XUN_SIDECAR_MAX_CLIENT_EVENT_LENGTH,
        XUN_SIDECAR_MAX_NICKNAME_BYTES, XUN_SIDECAR_PROTOCOL_VERSION, XunProfileFrame,
        XunProfileState,
    },
};
use p5136_profile::{CatalogInventory, CatalogXunProfile};
use thiserror::Error;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::watch,
    time::timeout,
};

const XUN_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
const XUN_WRITE_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Error)]
enum XunSidecarConnectionError {
    #[error("XUN sidecar handshake timed out")]
    HandshakeTimeout,

    #[error("XUN sidecar I/O failed")]
    Io(#[from] io::Error),

    #[error("invalid XUN sidecar handshake")]
    InvalidHandshake,

    #[error("invalid XUN sidecar nickname")]
    InvalidNickname,

    #[error("XUN sidecar profile write timed out")]
    WriteTimeout,

    #[error("XUN sidecar profile publisher stopped")]
    PublisherStopped,
}

#[derive(Debug, Default)]
struct XunSidecarRegistry {
    next_generation: u32,
    profiles: HashMap<String, watch::Sender<XunProfileFrame>>,
}

/// Nickname-scoped publisher for the optional XUN DLL transport.
///
/// Publication is synchronous and best-effort so an absent DLL can never
/// delay or fail a stock game request.
#[derive(Debug, Clone, Default)]
pub(crate) struct XunSidecarHandle {
    registry: Arc<Mutex<XunSidecarRegistry>>,
    data_raw_manifest: Option<DataRawManifest>,
}

impl XunSidecarHandle {
    pub(crate) fn new(data_raw_manifest: Option<DataRawManifest>) -> Self {
        Self {
            registry: Arc::new(Mutex::new(XunSidecarRegistry::default())),
            data_raw_manifest,
        }
    }

    fn lock(&self) -> MutexGuard<'_, XunSidecarRegistry> {
        self.registry
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn publish_catalog_profile(
        &self,
        nickname: &str,
        kart_id: u16,
        catalog: Option<&CatalogInventory>,
    ) {
        let mut frame = profile_for_kart(kart_id, catalog);
        let key = canonical_nickname_key(nickname);
        let mut registry = self.lock();
        registry.next_generation = registry.next_generation.wrapping_add(1).max(1);
        frame.generation = registry.next_generation;
        let sender = registry.profiles.entry(key).or_insert_with(|| {
            let (sender, _) = watch::channel(XunProfileFrame::disabled(kart_id));
            sender
        });
        sender.send_replace(frame);
        tracing::debug!(
            nickname,
            kart_id,
            exceed_type = frame.exceed_type,
            state = ?frame.state,
            generation = frame.generation,
            "published sidecar-only XUN physics profile"
        );
    }

    fn republish_current_profile(&self, nickname: &str) -> bool {
        let key = canonical_nickname_key(nickname);
        let mut registry = self.lock();
        let Some(mut frame) = registry.profiles.get(&key).map(|sender| *sender.borrow()) else {
            return false;
        };
        registry.next_generation = registry.next_generation.wrapping_add(1).max(1);
        frame.generation = registry.next_generation;
        registry
            .profiles
            .get(&key)
            .expect("the profile sender remains registered while its registry is locked")
            .send_replace(frame);
        true
    }

    fn subscribe(&self, nickname: &str) -> watch::Receiver<XunProfileFrame> {
        let key = canonical_nickname_key(nickname);
        let mut registry = self.lock();
        registry
            .profiles
            .entry(key)
            .or_insert_with(|| {
                let (sender, _) = watch::channel(XunProfileFrame::disabled(0));
                sender
            })
            .subscribe()
    }

    pub(crate) async fn serve_connection(&self, mut stream: TcpStream, peer: SocketAddr) {
        if let Err(error) = self.serve_connection_inner(&mut stream).await {
            tracing::warn!(%peer, %error, "XUN sidecar connection closed");
        }
    }

    async fn serve_connection_inner(
        &self,
        stream: &mut TcpStream,
    ) -> Result<(), XunSidecarConnectionError> {
        stream.set_nodelay(true)?;
        let mut magic = [0_u8; 4];
        timeout(XUN_HANDSHAKE_TIMEOUT, stream.read_exact(&mut magic))
            .await
            .map_err(|_| XunSidecarConnectionError::HandshakeTimeout)??;
        if magic == DATARAW_PREFLIGHT_REQUEST_MAGIC {
            return self.serve_dataraw_preflight(stream, magic).await;
        }
        let mut header = [0_u8; XUN_SIDECAR_HANDSHAKE_HEADER_LENGTH];
        header[0..4].copy_from_slice(&magic);
        timeout(XUN_HANDSHAKE_TIMEOUT, stream.read_exact(&mut header[4..]))
            .await
            .map_err(|_| XunSidecarConnectionError::HandshakeTimeout)??;
        if magic != XUN_SIDECAR_HANDSHAKE_MAGIC
            || u16::from_le_bytes([header[4], header[5]]) != XUN_SIDECAR_PROTOCOL_VERSION
        {
            return Err(XunSidecarConnectionError::InvalidHandshake);
        }
        let nickname_length = usize::from(u16::from_le_bytes([header[6], header[7]]));
        if nickname_length == 0 || nickname_length > XUN_SIDECAR_MAX_NICKNAME_BYTES {
            return Err(XunSidecarConnectionError::InvalidHandshake);
        }
        let mut nickname = vec![0_u8; nickname_length];
        timeout(XUN_HANDSHAKE_TIMEOUT, stream.read_exact(&mut nickname))
            .await
            .map_err(|_| XunSidecarConnectionError::HandshakeTimeout)??;
        let nickname = std::str::from_utf8(&nickname)
            .ok()
            .and_then(|nickname| normalize_nickname(nickname).ok())
            .ok_or(XunSidecarConnectionError::InvalidNickname)?;
        tracing::info!(nickname, "XUN sidecar subscribed to rider profile");
        let mut profiles = self.subscribe(&nickname);
        let (mut reader, mut writer) = stream.split();

        loop {
            let frame = *profiles.borrow_and_update();
            timeout(XUN_WRITE_TIMEOUT, writer.write_all(&frame.encode()))
                .await
                .map_err(|_| XunSidecarConnectionError::WriteTimeout)??;
            tokio::select! {
                changed = profiles.changed() => {
                    changed.map_err(|_| XunSidecarConnectionError::PublisherStopped)?;
                }
                event = read_client_event(&mut reader) => {
                    let event_type = event?;
                    if event_type == XUN_SIDECAR_CLIENT_EVENT_RACE_RESET {
                        let republished = self.republish_current_profile(&nickname);
                        tracing::debug!(
                            nickname,
                            republished,
                            "accepted XUN race-reset event and advanced the profile generation"
                        );
                    } else {
                        tracing::trace!(nickname, event_type, "accepted reserved XUN sidecar client event");
                    }
                }
            }
        }
    }

    async fn serve_dataraw_preflight(
        &self,
        stream: &mut TcpStream,
        magic: [u8; 4],
    ) -> Result<(), XunSidecarConnectionError> {
        let mut request = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
        request[0..4].copy_from_slice(&magic);
        timeout(XUN_HANDSHAKE_TIMEOUT, stream.read_exact(&mut request[4..]))
            .await
            .map_err(|_| XunSidecarConnectionError::HandshakeTimeout)??;
        let client =
            decode_dataraw_request(&request).ok_or(XunSidecarConnectionError::InvalidHandshake)?;
        let status = match self.data_raw_manifest {
            None => DataRawPreflightStatus::ServerDisabled,
            Some(server) if server == client => DataRawPreflightStatus::Match,
            Some(_) => DataRawPreflightStatus::ManifestMismatch,
        };
        let response = encode_dataraw_response(status, self.data_raw_manifest);
        timeout(XUN_WRITE_TIMEOUT, stream.write_all(&response))
            .await
            .map_err(|_| XunSidecarConnectionError::WriteTimeout)??;
        tracing::info!(
            ?status,
            client_files = client.file_count,
            server_files = self.data_raw_manifest.map(|manifest| manifest.file_count),
            "completed DataRaw file-list preflight"
        );
        Ok(())
    }
}

async fn read_client_event(
    reader: &mut tokio::net::tcp::ReadHalf<'_>,
) -> Result<u16, XunSidecarConnectionError> {
    let mut header = [0_u8; XUN_SIDECAR_CLIENT_EVENT_HEADER_LENGTH];
    reader.read_exact(&mut header).await?;
    let frame_length = usize::from(u16::from_le_bytes([header[6], header[7]]));
    if header[0..4] != XUN_SIDECAR_CLIENT_EVENT_MAGIC
        || u16::from_le_bytes([header[4], header[5]]) != XUN_SIDECAR_PROTOCOL_VERSION
        || !(XUN_SIDECAR_CLIENT_EVENT_HEADER_LENGTH..=XUN_SIDECAR_MAX_CLIENT_EVENT_LENGTH)
            .contains(&frame_length)
        || header[10..12] != [0, 0]
    {
        return Err(XunSidecarConnectionError::InvalidHandshake);
    }
    let event_type = u16::from_le_bytes([header[8], header[9]]);
    let mut payload = vec![0_u8; frame_length - XUN_SIDECAR_CLIENT_EVENT_HEADER_LENGTH];
    reader.read_exact(&mut payload).await?;
    // Event types are reserved for a later room/generation-fenced relay. The
    // bounded frame is consumed now so old servers remain compatible with a
    // future DLL, but no gameplay effect is published yet.
    Ok(event_type)
}

fn profile_for_kart(kart_id: u16, catalog: Option<&CatalogInventory>) -> XunProfileFrame {
    profile_from_catalog_xun(
        kart_id,
        catalog.and_then(|catalog| catalog.kart_xun_profile(kart_id)),
    )
}

fn profile_from_catalog_xun(kart_id: u16, profile: Option<CatalogXunProfile>) -> XunProfileFrame {
    let Some(profile) = profile else {
        return XunProfileFrame::disabled(kart_id);
    };
    if profile.is_item_profile() {
        return XunProfileFrame {
            kart_id,
            exceed_type: profile.exceed_type,
            state: XunProfileState::ItemMode,
            default_engine_type: profile.default_engine_type,
            default_handle_type: profile.default_handle_type,
            default_wheel_type: profile.default_wheel_type,
            default_booster_type: profile.default_booster_type,
            ..XunProfileFrame::disabled(kart_id)
        };
    }
    let Some((booster_use_count, use_time_ms)) = profile.supported_speed_timing() else {
        return XunProfileFrame {
            kart_id,
            exceed_type: profile.exceed_type,
            state: XunProfileState::UnsupportedSpecial,
            default_engine_type: profile.default_engine_type,
            default_handle_type: profile.default_handle_type,
            default_wheel_type: profile.default_wheel_type,
            default_booster_type: profile.default_booster_type,
            ..XunProfileFrame::disabled(kart_id)
        };
    };
    XunProfileFrame {
        generation: 0,
        kart_id,
        exceed_type: profile.exceed_type,
        state: XunProfileState::SupportedSpeed,
        flags: XUN_PROFILE_FLAG_SPEED_BOOST_GAUGE | XUN_PROFILE_FLAG_REMAINING_CONSUMERS,
        booster_use_count,
        use_time_ms,
        charge_boost_by_speed_multiplier: 350.0,
        drift_gauge_factor: 2.0,
        wall_gauge_added: 0.09,
        boost_gauge_added: 0.03,
        anti_collide_balance: 0.8,
        default_engine_type: profile.default_engine_type,
        default_handle_type: profile.default_handle_type,
        default_wheel_type: profile.default_wheel_type,
        default_booster_type: profile.default_booster_type,
    }
}

#[cfg(test)]
mod tests {
    use super::{XunSidecarHandle, profile_for_kart, profile_from_catalog_xun};
    use p5136_core::{
        dataraw_manifest::{
            DATARAW_PREFLIGHT_FRAME_LENGTH, DataRawManifest, DataRawPreflightStatus,
            decode_dataraw_response, encode_dataraw_request,
        },
        xun_sidecar_protocol::{
            XUN_SIDECAR_CLIENT_EVENT_MAGIC, XUN_SIDECAR_CLIENT_EVENT_RACE_RESET,
            XUN_SIDECAR_PROFILE_FRAME_LENGTH, XunProfileState, encode_xun_sidecar_handshake,
        },
    };
    use p5136_profile::CatalogXunProfile;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::{TcpListener, TcpStream},
    };

    #[test]
    fn missing_catalog_is_fail_closed() {
        let frame = profile_for_kart(3_000, None);
        assert_eq!(frame.state, XunProfileState::Disabled);
        assert_eq!(frame.flags, 0);
        assert_eq!(frame.encode().len(), XUN_SIDECAR_PROFILE_FRAME_LENGTH);
    }

    #[test]
    fn only_baseline_speed_types_receive_active_consumer_profiles() {
        for (exceed_type, boosters, duration) in [(2, 4, 3_000), (3, 5, 3_750), (4, 6, 4_500)] {
            let frame = profile_from_catalog_xun(
                2_000 + u16::from(exceed_type),
                Some(CatalogXunProfile {
                    exceed_type,
                    ..CatalogXunProfile::default()
                }),
            );
            assert_eq!(frame.state, XunProfileState::SupportedSpeed);
            assert_eq!(frame.booster_use_count, boosters);
            assert_eq!(frame.use_time_ms, duration);
            assert_ne!(frame.flags, 0);
        }

        let item = profile_from_catalog_xun(
            2_100,
            Some(CatalogXunProfile {
                exceed_type: 1,
                ..CatalogXunProfile::default()
            }),
        );
        assert_eq!(item.state, XunProfileState::ItemMode);
        assert_eq!(item.flags, 0);

        let special = profile_from_catalog_xun(
            2_101,
            Some(CatalogXunProfile {
                exceed_type: 7,
                ..CatalogXunProfile::default()
            }),
        );
        assert_eq!(special.state, XunProfileState::UnsupportedSpecial);
        assert_eq!(special.flags, 0);
    }

    #[tokio::test]
    async fn nickname_channel_delivers_profiles_and_accepts_reserved_client_frames() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let sidecar = XunSidecarHandle::default();
        let serving = sidecar.clone();
        let task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            serving.serve_connection(stream, peer).await;
        });

        let mut client = TcpStream::connect(endpoint).await.unwrap();
        client
            .write_all(&encode_xun_sidecar_handshake("다오").unwrap())
            .await
            .unwrap();
        let mut frame = [0_u8; XUN_SIDECAR_PROFILE_FRAME_LENGTH];
        client.read_exact(&mut frame).await.unwrap();
        assert_eq!(&frame[0..4], b"P5XP");
        assert_eq!(u16::from_le_bytes(frame[12..14].try_into().unwrap()), 0);

        sidecar.publish_catalog_profile("다오", 777, None);
        client.read_exact(&mut frame).await.unwrap();
        assert_eq!(u16::from_le_bytes(frame[12..14].try_into().unwrap()), 777);
        assert_eq!(frame[15], XunProfileState::Disabled as u8);

        let mut reserved = [0_u8; 12];
        reserved[0..4].copy_from_slice(&XUN_SIDECAR_CLIENT_EVENT_MAGIC);
        reserved[4..6].copy_from_slice(
            &p5136_core::xun_sidecar_protocol::XUN_SIDECAR_PROTOCOL_VERSION.to_le_bytes(),
        );
        reserved[6..8].copy_from_slice(&12_u16.to_le_bytes());
        reserved[8..10].copy_from_slice(&9_u16.to_le_bytes());
        client.write_all(&reserved).await.unwrap();
        client.read_exact(&mut frame).await.unwrap();
        assert_eq!(u16::from_le_bytes(frame[12..14].try_into().unwrap()), 777);

        reserved[8..10].copy_from_slice(&XUN_SIDECAR_CLIENT_EVENT_RACE_RESET.to_le_bytes());
        client.write_all(&reserved).await.unwrap();
        client.read_exact(&mut frame).await.unwrap();
        assert_eq!(u32::from_le_bytes(frame[8..12].try_into().unwrap()), 2);
        assert_eq!(u16::from_le_bytes(frame[12..14].try_into().unwrap()), 777);

        drop(client);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn dataraw_preflight_shares_the_port_without_entering_xun_subscription() {
        let expected = DataRawManifest {
            file_count: 77,
            list_digest: [0x5a; 32],
        };
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let endpoint = listener.local_addr().unwrap();
        let sidecar = XunSidecarHandle::new(Some(expected));
        let task = tokio::spawn(async move {
            let (stream, peer) = listener.accept().await.unwrap();
            sidecar.serve_connection(stream, peer).await;
        });

        let mut client = TcpStream::connect(endpoint).await.unwrap();
        client
            .write_all(&encode_dataraw_request(expected))
            .await
            .unwrap();
        let mut response = [0_u8; DATARAW_PREFLIGHT_FRAME_LENGTH];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(
            decode_dataraw_response(&response),
            Some((DataRawPreflightStatus::Match, Some(expected)))
        );
        task.await.unwrap();
    }
}
