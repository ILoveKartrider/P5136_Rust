use std::{
    array,
    future::Future,
    io,
    mem::size_of,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    pin::Pin,
    sync::Arc,
    time::Instant,
};

use chrono::{Local, NaiveDate, Timelike};
use p5136_core::{
    adler32,
    channel::{
        ChannelError, parse_pq_channel_movein, parse_pq_channel_switch, resolve_channel_id,
        serialize_pr_channel_move_in, serialize_pr_channel_switch,
    },
    equipment_protocol::{
        EquipmentProtocolError, EquipmentRequest, PlantPartEquipRequest, RiderItemSelection,
        classify_equipment_request, parse_equip_plant_part, parse_set_rider_items,
        serialize_equip_tuning_failure, serialize_equip_tuning_success,
    },
    frame::{self, FrameError},
    handshake,
    inventory::{InventoryError, serialize_get_rider_sequence},
    kart_physics::{
        KartPhysicsBuildError, P5136KartPhysicsSnapshot, build_p5136_kart_physics_block,
    },
    lobby_protocol::{
        LobbyProtocolError, LobbyRequest, classify_lobby_request, parse_change_master_request,
        parse_change_team_request, parse_set_slot_state_request, parse_start_room_request,
    },
    login::{
        LegacyTime, LoginError, PrLoginFields, parse_pq_login, serialize_pr_cn_authen_login,
        serialize_pr_login,
    },
    myroom_protocol::{
        MYROOM_ITEM_CHUNK_SIZE, MyRoomInfo, MyRoomPlayerSlot, MyRoomProtocolError, MyRoomRequest,
        classify_myroom_request, parse_first_request, parse_request_items, parse_secede_request,
        parse_update_info, plan_owner_item_packets, serialize_myroom_info,
        serialize_owner_item_enchants, serialize_owner_items,
    },
    packet::PacketError,
    race_protocol::{
        RaceProtocolError, RaceRequest, classify_race_request, parse_ai_goal_in_request,
        parse_game_control_request, parse_team_booster_request,
    },
    race_start_protocol::P5136KartPhysicsBlock,
    room_protocol::{
        RoomPlayer, RoomProtocolError, RoomProtocolRequest, classify_room_protocol_request,
        parse_ch_create_room_request, parse_ch_get_room_list_request, parse_ch_join_room_request,
        parse_ch_leave_room_request, parse_gr_first_request,
    },
    startup::{
        self, PrGetRiderFields, RIDER_ITEM_SNAPSHOT_WIRE_LENGTH, StartupError, StartupRequest,
        classify_startup_request, is_startup_noop, parse_pq_update_game_option,
    },
    track::P5136_FALLBACK_TRACK_ID,
};
use p5136_profile::{
    CatalogInventory, EquipmentExceptions, EquipmentStateError, InventoryBuildError,
    MAX_MYROOM_ITEM_RECORDS, MyRoomItemStateError, MyRoomOwnerInventory, Profile,
    ProfileStoreError, build_inventory_snapshot_with_equipment, rider_item_snapshot,
};
use rand::Rng;
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, oneshot},
    time,
};

use crate::{
    ChannelBinding, IdentityBinding, IdentityGeneration, MigrationToken, ServerConfig, SessionId,
    UserNo, WorldError, WorldHandle,
    equipment_persistence::{
        PreparedRiderEquipmentWrite, RiderEquipmentPersistError, RiderEquipmentValidationError,
        RiderEquipmentWriteError, catalog_grants, kart_is_owned,
    },
    identity::IdentityOperationLease,
    myroom_hub::{MyRoomWirePlan, MyRoomWireProjection},
    myroom_persistence::{
        MYROOM_INFO_WRITE_OPERATION, MyRoomCompletionSlot, MyRoomInfoWriteError,
        MyRoomInfoWriteReceipt,
    },
    operation_gate::{WireOperationGate, WireOperationGuard},
    profile_io::{
        MyRoomProfileLease, ProfileIoError, ProfileIoHandle, ProfileJobAdmission,
        ProfileLanePermit, myroom_profile_presentation,
    },
    world::{
        AdmittedWorldHandle, LobbyCommandPayload, LobbyError, MyRoomCommandPayload,
        MyRoomOwnerItemLoad, MyRoomSessionRole, OutboundBatch, RaceCommandPayload,
        RoomCommandPayload, RoomParticipant, StartRoomPlan,
    },
};

const MAX_OUTBOUND_BATCH_BURST: usize = 8;
/// A single owner-item request is one ordered TCP write batch. Capping it at
/// the exact loader maximum keeps the response bounded without rejecting any
/// valid combination of the three independently bounded C# sidecars.
const MAX_MYROOM_OWNER_ITEM_PACKETS: usize =
    3 * MAX_MYROOM_ITEM_RECORDS.div_ceil(MYROOM_ITEM_CHUNK_SIZE);
const MAX_MYROOM_OWNER_ITEM_BYTES: usize = 8 * 1024 * 1024;
const MAX_MYROOM_WIRE_PLAN_ATTEMPTS: usize = 3;
const MYROOM_OWNER_ITEM_READ_OPERATION: &str = "load MyRoom owner items";

enum SessionReadEvent {
    Outbound(Option<OutboundBatch>),
    Frame(Result<Vec<u8>, LoginSessionError>),
}

/// One decoded request owns both shutdown admission and, once authenticated,
/// an exact identity-generation lease. Neither capability is cloneable.
#[derive(Debug)]
struct SessionFrameOperation {
    #[expect(dead_code, reason = "drop retires global graceful-request admission")]
    wire: WireOperationGuard,
    identity: Option<IdentityOperationLease>,
}

impl SessionFrameOperation {
    fn new(wire: WireOperationGuard, identity: Option<IdentityOperationLease>) -> Self {
        Self { wire, identity }
    }

    fn identity(&self) -> Option<&IdentityOperationLease> {
        self.identity.as_ref()
    }
}

async fn select_session_read_event<F>(
    cancellation: &mut oneshot::Receiver<()>,
    outbound: &mut mpsc::Receiver<OutboundBatch>,
    mut frame: Pin<&mut F>,
    prioritize_frame: bool,
) -> Result<SessionReadEvent, LoginSessionError>
where
    F: Future<Output = Result<Vec<u8>, LoginSessionError>>,
{
    if prioritize_frame {
        tokio::select! {
            biased;
            _ = cancellation => Err(LoginSessionError::Superseded),
            result = frame.as_mut() => Ok(SessionReadEvent::Frame(result)),
            batch = outbound.recv() => Ok(SessionReadEvent::Outbound(batch)),
        }
    } else {
        tokio::select! {
            biased;
            _ = cancellation => Err(LoginSessionError::Superseded),
            event = async {
                tokio::select! {
                    batch = outbound.recv() => SessionReadEvent::Outbound(batch),
                    result = frame.as_mut() => SessionReadEvent::Frame(result),
                }
            } => Ok(event),
        }
    }
}

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
    StartupProtocol(#[from] StartupError),

    #[error(transparent)]
    ProfileStore(#[from] ProfileStoreError),

    #[error(transparent)]
    EquipmentState(#[from] EquipmentStateError),

    #[error(transparent)]
    InventoryBuild(#[from] InventoryBuildError),

    #[error(transparent)]
    InventoryProtocol(#[from] InventoryError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error(transparent)]
    LobbyProtocol(#[from] LobbyProtocolError),

    #[error(transparent)]
    RaceProtocol(#[from] RaceProtocolError),

    #[error(transparent)]
    KartPhysicsBuild(#[from] KartPhysicsBuildError),

    #[error(transparent)]
    EquipmentProtocol(#[from] EquipmentProtocolError),

    #[error(transparent)]
    MyRoomProtocol(#[from] MyRoomProtocolError),

    #[error(transparent)]
    MyRoomItemState(#[from] MyRoomItemStateError),

    #[error("live MyRoom wire projection failed")]
    MyRoomWireProjection {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("MyRoom owner-info write failed")]
    MyRoomInfoWrite {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error(transparent)]
    ProfileIo(#[from] ProfileIoError),

    #[error("rider-equipment persistence or publication failed")]
    RiderEquipmentWrite {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error(
        "rider-equipment reload for {nickname:?} returned revision {actual:?} behind durable revision {durable}"
    )]
    RiderEquipmentReloadBehind {
        nickname: String,
        durable: u64,
        actual: Option<u64>,
    },

    #[error(transparent)]
    World(#[from] WorldError),

    #[error("client did not complete login before the login timeout")]
    LoginTimeout,

    #[error("authenticated login session exceeded its idle timeout")]
    SessionIdleTimeout,

    #[error("login session write exceeded its timeout")]
    WriteTimeout,

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

    #[error("the world actor closed the login session's outbound queue")]
    OutboundClosed,

    #[error("the session has no profile bound to its current identity generation")]
    ProfileNotBound,

    #[error("PqGetRider requires a configured and validated inventory catalog")]
    CatalogUnavailable,

    #[error("rider item {item_id} in category {category} is not granted by the P5136 inventory")]
    RiderItemNotGranted { category: u16, item_id: u16 },

    #[error("kart {kart_id} serial {serial} is not granted by the P5136 inventory")]
    KartNotGranted { kart_id: u16, serial: u16 },

    #[error("the bound profile path has no rider directory")]
    ProfileDirectoryUnavailable,

    #[error("profile {nickname:?} does not exist and remote profile creation is disabled")]
    ProfileCreationDenied { nickname: String },

    #[error(
        "profile admission for {admitted:?} cannot authorize operation on profile {requested:?}"
    )]
    ProfileSubjectMismatch { admitted: String, requested: String },

    #[error("MyRoom owner-item response has {actual} packets; operational maximum is {maximum}")]
    MyRoomOwnerItemPacketLimit { actual: usize, maximum: usize },

    #[error("MyRoom owner-item response has {actual} bytes; operational maximum is {maximum}")]
    MyRoomOwnerItemByteLimit { actual: usize, maximum: usize },

    #[error("MyRoom owner-item response byte length overflowed usize")]
    MyRoomOwnerItemByteLengthOverflow,

    #[error(
        "MyRoom owner-item serializer diverged from its checked wire plan: planned {planned_packets} packets/{planned_bytes} bytes, produced {actual_packets} packets/{actual_bytes} bytes"
    )]
    MyRoomOwnerItemWirePlanMismatch {
        planned_packets: usize,
        planned_bytes: usize,
        actual_packets: usize,
        actual_bytes: usize,
    },

    #[error(
        "identity changed while profile I/O was in flight: expected session {expected_owner:?}, user {expected_user_no:?}, generation {expected_generation:?}; received session {actual_owner:?}, user {actual_user_no:?}, generation {actual_generation:?}"
    )]
    ProfileIdentityFenceChanged {
        expected_owner: SessionId,
        expected_user_no: UserNo,
        expected_generation: IdentityGeneration,
        actual_owner: SessionId,
        actual_user_no: UserNo,
        actual_generation: IdentityGeneration,
    },
}

/// Shared persistence and ownership-transfer coordination.
///
/// Disk operations and migration completion take the same canonical,
/// nickname-keyed lane. This prevents an old generation from publishing a
/// profile revision while a destination session takes ownership, without
/// serializing unrelated riders. Filesystem work runs on the tracked blocking
/// profile runtime.
#[derive(Debug, Clone)]
pub(crate) struct ProfileCoordinator {
    io: ProfileIoHandle,
    catalog: Option<Arc<CatalogInventory>>,
    #[cfg(test)]
    blocking_update_hook: Option<Arc<BlockingUpdateHook>>,
    #[cfg(test)]
    blocking_owner_item_hook: Option<Arc<BlockingUpdateHook>>,
}

/// A fully serialized owner-item response that has passed the server's
/// aggregate packet-count and byte-size limits.
#[derive(Debug, PartialEq, Eq)]
struct MyRoomOwnerItemPacketBatch {
    packets: Vec<Vec<u8>>,
}

impl MyRoomOwnerItemPacketBatch {
    fn from_inventory(
        inventory: &MyRoomOwnerInventory,
        prevent_item: bool,
    ) -> Result<Self, LoginSessionError> {
        let plan = plan_owner_item_packets(
            inventory.tunes.len(),
            inventory.karts.len(),
            inventory.parts.len(),
        )?;
        Self::enforce_wire_plan(
            plan.packet_count(),
            plan.byte_len(),
            MAX_MYROOM_OWNER_ITEM_PACKETS,
            MAX_MYROOM_OWNER_ITEM_BYTES,
        )?;

        // Both operational limits have been checked before either serializer
        // allocates its packet buffers.
        let mut packets = serialize_owner_item_enchants(&inventory.tunes)?;
        packets.extend(serialize_owner_items(
            &inventory.karts,
            &inventory.parts,
            prevent_item,
        )?);
        let actual_packets = packets.len();
        let actual_bytes = packets
            .iter()
            .try_fold(0_usize, |total, packet| total.checked_add(packet.len()));
        let actual_bytes =
            actual_bytes.ok_or(LoginSessionError::MyRoomOwnerItemByteLengthOverflow)?;
        if actual_packets != plan.packet_count() || actual_bytes != plan.byte_len() {
            return Err(LoginSessionError::MyRoomOwnerItemWirePlanMismatch {
                planned_packets: plan.packet_count(),
                planned_bytes: plan.byte_len(),
                actual_packets,
                actual_bytes,
            });
        }
        Ok(Self { packets })
    }

    fn enforce_wire_plan(
        packet_count: usize,
        byte_len: usize,
        maximum_packets: usize,
        maximum_bytes: usize,
    ) -> Result<(), LoginSessionError> {
        if packet_count > maximum_packets {
            return Err(LoginSessionError::MyRoomOwnerItemPacketLimit {
                actual: packet_count,
                maximum: maximum_packets,
            });
        }
        if byte_len > maximum_bytes {
            return Err(LoginSessionError::MyRoomOwnerItemByteLimit {
                actual: byte_len,
                maximum: maximum_bytes,
            });
        }
        Ok(())
    }

    fn into_packets(self) -> Vec<Vec<u8>> {
        self.packets
    }
}

#[cfg(test)]
#[derive(Debug)]
struct BlockingUpdateHook {
    entered: std::sync::Barrier,
    release: std::sync::Barrier,
}

#[cfg(test)]
impl BlockingUpdateHook {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            entered: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        })
    }
}

impl ProfileCoordinator {
    #[must_use]
    pub(crate) fn new(io: ProfileIoHandle, catalog: Option<Arc<CatalogInventory>>) -> Self {
        Self {
            io,
            catalog,
            #[cfg(test)]
            blocking_update_hook: None,
            #[cfg(test)]
            blocking_owner_item_hook: None,
        }
    }

    #[cfg(test)]
    fn new_test(
        root: PathBuf,
        catalog: Option<Arc<CatalogInventory>>,
    ) -> (Self, crate::profile_io::ProfileIoRuntime) {
        let limits = crate::profile_io::ProfileIoLimits::for_tests(128, 128);
        let bootstrap = crate::profile_io::ProfileIoBootstrap::acquire(root, limits)
            .expect("test profile runtime should acquire its isolated race-run lease");
        let (io, runtime) = bootstrap.spawn();
        (Self::new(io, catalog), runtime)
    }

    fn catalog(&self) -> Option<&CatalogInventory> {
        self.catalog.as_deref()
    }

    #[cfg(test)]
    fn with_blocking_update_hook(mut self, hook: Arc<BlockingUpdateHook>) -> Self {
        self.blocking_update_hook = Some(hook);
        self
    }

    #[cfg(test)]
    fn with_blocking_owner_item_hook(mut self, hook: Arc<BlockingUpdateHook>) -> Self {
        self.blocking_owner_item_hook = Some(hook);
        self
    }

    async fn admit(
        &self,
        nickname: &str,
        operation: &'static str,
    ) -> Result<ProfileJobAdmission, LoginSessionError> {
        Ok(self.io.admit(nickname, operation).await?)
    }

    async fn admit_for_operation(
        &self,
        identity_operation: &IdentityOperationLease,
        nickname: &str,
        operation: &'static str,
    ) -> Result<ProfileJobAdmission, LoginSessionError> {
        let retained = identity_operation.try_retain().map_err(WorldError::from)?;
        Ok(self
            .io
            .admit(nickname, operation)
            .await?
            .retain_identity_operation(retained))
    }

    fn ensure_admitted_subject(
        admission: &ProfileJobAdmission,
        nickname: &str,
    ) -> Result<(), LoginSessionError> {
        if admission
            .subject()
            .matches_nickname(nickname)
            .map_err(ProfileIoError::from)?
        {
            return Ok(());
        }
        Err(LoginSessionError::ProfileSubjectMismatch {
            admitted: admission.subject().nickname().to_owned(),
            requested: nickname.to_owned(),
        })
    }

    async fn load(
        &self,
        nickname: String,
        allow_creation: bool,
        admission: ProfileJobAdmission,
    ) -> Result<(ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        let completed = admission
            .run("load profile", move |store, _, subject| {
                if !allow_creation && !store.profile_exists(subject.nickname())? {
                    return Err(LoginSessionError::ProfileCreationDenied { nickname });
                }
                Ok::<_, LoginSessionError>(store.load_or_create(subject.nickname())?)
            })
            .await?;
        let (loaded, lane) = completed.into_parts();
        let loaded = loaded?;
        Ok((
            ProfileSnapshot {
                profile: loaded.profile,
                revision: loaded.revision,
                source_path: loaded.source_path,
            },
            lane,
        ))
    }

    /// Reloads each occupied `MyRoom` slot through its own canonical profile
    /// lane. Lanes are released one at a time: holding several rider lanes
    /// together would permit an A-visits-B/B-visits-A deadlock.
    async fn load_myroom_wire_projection_for_operation(
        &self,
        operation: &IdentityOperationLease,
        plan: &MyRoomWirePlan,
    ) -> Result<MyRoomWireProjection, LoginSessionError> {
        let identities = plan
            .slot_identities()
            .map(Option::<&IdentityBinding>::cloned)
            .collect::<Vec<_>>();
        let mut players: [Option<MyRoomPlayerSlot>;
            p5136_core::myroom_protocol::MYROOM_SLOT_COUNT] = array::from_fn(|_| None);
        for (slot, identity) in identities.into_iter().enumerate() {
            let Some(identity) = identity else {
                continue;
            };
            let admission = self
                .admit_for_operation(
                    operation,
                    &identity.nickname,
                    "load live MyRoom wire profile",
                )
                .await?;
            let (profile, lane) = self
                .load(identity.nickname.clone(), false, admission)
                .await?;
            players[slot] =
                Some(myroom_profile_presentation(&profile.profile).player_for(&identity));
            drop(lane);
        }
        plan.project(players)
            .map_err(|source| LoginSessionError::MyRoomWireProjection {
                source: Box::new(source),
            })
    }

    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "consumed by the pending MyRoom session-dispatch integration"
        )
    )]
    async fn load_myroom_owner_profile(
        &self,
        nickname: String,
        admission: ProfileJobAdmission,
    ) -> Result<(ProfileSnapshot, MyRoomInfo, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        let completed = admission
            .run("load MyRoom owner profile", move |store, _, subject| {
                if !store.profile_exists(subject.nickname())? {
                    return Err(LoginSessionError::ProfileCreationDenied { nickname });
                }
                let loaded = store.load_or_create(subject.nickname())?;
                let info = loaded.profile.my_room.try_to_protocol_info()?;
                Ok::<_, LoginSessionError>((
                    ProfileSnapshot {
                        profile: loaded.profile,
                        revision: loaded.revision,
                        source_path: loaded.source_path,
                    },
                    info,
                ))
            })
            .await?;
        let (loaded, lane) = completed.into_parts();
        let (profile, info) = loaded?;
        Ok((profile, info, lane))
    }

    async fn load_myroom_owner_items(
        &self,
        nickname: String,
        admission: ProfileJobAdmission,
    ) -> Result<(MyRoomOwnerItemPacketBatch, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        #[cfg(test)]
        let blocking_owner_item_hook = self.blocking_owner_item_hook.clone();
        let completed = admission
            .run("load MyRoom owner items", move |store, _, subject| {
                #[cfg(test)]
                if let Some(hook) = blocking_owner_item_hook {
                    hook.entered.wait();
                    hook.release.wait();
                }
                if !store.profile_exists(subject.nickname())? {
                    return Err(LoginSessionError::ProfileCreationDenied { nickname });
                }
                let loaded = store.load_or_create(subject.nickname())?;
                let rider_directory = loaded
                    .source_path
                    .parent()
                    .map(std::path::Path::to_owned)
                    .ok_or(LoginSessionError::ProfileDirectoryUnavailable)?;
                let inventory = MyRoomOwnerInventory::load(rider_directory)?;
                // The C# server reads a race-prone process-global value that
                // is overwritten by whichever profile loaded last. Binding
                // this flag to the requested owner is the deterministic Rust
                // compatibility policy.
                MyRoomOwnerItemPacketBatch::from_inventory(
                    &inventory,
                    loaded.profile.server_setting.prevent_item_use != 0,
                )
            })
            .await?;
        let (packets, lane) = completed.into_parts();
        Ok((packets?, lane))
    }

    async fn update_game_options(
        &self,
        nickname: String,
        options: startup::GameOptions,
        admission: ProfileJobAdmission,
    ) -> Result<(ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run("update game options", move |store, _, subject| {
                #[cfg(test)]
                if let Some(hook) = blocking_update_hook {
                    hook.entered.wait();
                    hook.release.wait();
                }
                store.update(subject.nickname(), |profile| {
                    apply_game_options(&mut profile.game_option, &options);
                })
            })
            .await?;
        let (updated, lane) = completed.into_parts();
        let (saved, profile) = updated?;
        Ok((
            ProfileSnapshot {
                profile,
                revision: Some(saved.revision),
                source_path: saved.path,
            },
            lane,
        ))
    }

    fn prepare_rider_equipment_write(
        &self,
        selection: RiderItemSelection,
        admission: ProfileJobAdmission,
        completion: MyRoomCompletionSlot,
    ) -> Result<PreparedRiderEquipmentWrite, LoginSessionError> {
        let catalog = self
            .catalog
            .clone()
            .ok_or(LoginSessionError::CatalogUnavailable)?;
        let prepared = PreparedRiderEquipmentWrite::new(admission, selection, catalog, completion);
        #[cfg(test)]
        let prepared = if let Some(hook) = self.blocking_update_hook.clone() {
            prepared.with_test_hook(Arc::new(move || {
                hook.entered.wait();
                hook.release.wait();
            }))
        } else {
            prepared
        };
        Ok(prepared)
    }

    async fn equip_plant_part(
        &self,
        request: PlantPartEquipRequest,
        admission: ProfileJobAdmission,
    ) -> Result<(bool, ProfileLanePermit), LoginSessionError> {
        let catalog = self.catalog.clone();
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run("equip plant part", move |store, _, subject| {
                #[cfg(test)]
                if let Some(hook) = blocking_update_hook {
                    hook.entered.wait();
                    hook.release.wait();
                }
                let loaded = store.load_or_create(subject.nickname())?;
                if !plant_part_is_owned(catalog.as_deref(), &loaded.profile, request) {
                    return Ok::<_, LoginSessionError>(false);
                }
                let rider_directory = loaded
                    .source_path
                    .parent()
                    .map(std::path::Path::to_owned)
                    .ok_or(LoginSessionError::ProfileDirectoryUnavailable)?;
                EquipmentExceptions::equip_plant_part(rider_directory, request)?;
                Ok(true)
            })
            .await?;
        let (result, lane) = completed.into_parts();
        Ok((result?, lane))
    }

    async fn get_rider_sequence(
        &self,
        nickname: String,
        admission: ProfileJobAdmission,
    ) -> Result<(Vec<Vec<u8>>, ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        let catalog = self
            .catalog
            .clone()
            .ok_or(LoginSessionError::CatalogUnavailable)?;
        let completed = admission
            .run("load fresh rider inventory", move |store, _, subject| {
                let mut loaded = store.load_or_create(subject.nickname())?;
                if loaded.profile.rider_item.kart != 0 && loaded.profile.rider_item.kart_serial == 0
                {
                    let (saved, profile) = store.update(subject.nickname(), |profile| {
                        if profile.rider_item.kart != 0 && profile.rider_item.kart_serial == 0 {
                            profile.rider_item.kart_serial = 1;
                        }
                    })?;
                    loaded.profile = profile;
                    loaded.revision = Some(saved.revision);
                    loaded.source_path = saved.path;
                }
                let rider_directory = loaded
                    .source_path
                    .parent()
                    .map(std::path::Path::to_owned)
                    .ok_or(LoginSessionError::ProfileDirectoryUnavailable)?;
                let equipment = EquipmentExceptions::load(store.root(), rider_directory)?;
                let inventory =
                    build_inventory_snapshot_with_equipment(&catalog, &loaded.profile, equipment)?;
                let rider = profile_rider_fields(nickname, &loaded.profile);
                let responses = serialize_get_rider_sequence(&inventory, &rider)?
                    .into_iter()
                    .map(|packet| packet.logical_packet)
                    .collect();
                Ok::<_, LoginSessionError>((
                    responses,
                    ProfileSnapshot {
                        profile: loaded.profile,
                        revision: loaded.revision,
                        source_path: loaded.source_path,
                    },
                ))
            })
            .await?;
        let (result, lane) = completed.into_parts();
        let (responses, profile) = result?;
        Ok((responses, profile, lane))
    }
}

fn plant_part_is_owned(
    catalog: Option<&CatalogInventory>,
    profile: &Profile,
    request: PlantPartEquipRequest,
) -> bool {
    let Some(catalog) = catalog else {
        return false;
    };
    if request.kart_category != 3 {
        return false;
    }
    let Ok(kart_id) = u16::try_from(request.kart_id) else {
        return false;
    };
    let Ok(kart_serial) = u16::try_from(request.kart_serial) else {
        return false;
    };
    if !kart_is_owned(catalog, profile, kart_id, kart_serial) {
        return false;
    }
    request.item_id == 0
        || u16::try_from(request.item_category)
            .ok()
            .zip(u16::try_from(request.item_id).ok())
            .is_some_and(|(category, item_id)| catalog_grants(catalog, category, item_id))
}

#[derive(Debug, Clone)]
struct ProfileSnapshot {
    profile: Profile,
    revision: Option<u64>,
    source_path: PathBuf,
}

#[derive(Debug, Default)]
struct SessionContext {
    profile: Option<BoundProfile>,
}

impl SessionContext {
    fn is_authenticated(&self) -> bool {
        self.profile.is_some()
    }

    fn bind_profile(&mut self, identity: IdentityBinding, profile: ProfileSnapshot) {
        tracing::trace!(
            nickname = %identity.nickname,
            revision = ?profile.revision,
            source_path = %profile.source_path.display(),
            "binding profile snapshot to session generation"
        );
        self.profile = Some(BoundProfile { identity, profile });
    }

    fn bound_identity(&self) -> Result<&IdentityBinding, LoginSessionError> {
        self.profile
            .as_ref()
            .map(|bound| &bound.identity)
            .ok_or(LoginSessionError::ProfileNotBound)
    }

    fn profile_for(&self, identity: &IdentityBinding) -> Result<&Profile, LoginSessionError> {
        Ok(&self.bound_profile_for(identity)?.profile.profile)
    }

    fn bound_profile_for(
        &self,
        identity: &IdentityBinding,
    ) -> Result<&BoundProfile, LoginSessionError> {
        self.profile
            .as_ref()
            .filter(|bound| bound.identity.owner == identity.owner)
            .filter(|bound| bound.identity.user_no == identity.user_no)
            .filter(|bound| bound.identity.generation == identity.generation)
            .ok_or(LoginSessionError::ProfileNotBound)
    }

    fn apply_myroom_info_write(
        &mut self,
        identity: &IdentityBinding,
        receipt: &MyRoomInfoWriteReceipt,
    ) -> Result<(), LoginSessionError> {
        let bound = self
            .profile
            .as_mut()
            .filter(|bound| bound.identity.owner == identity.owner)
            .filter(|bound| bound.identity.user_no == identity.user_no)
            .filter(|bound| bound.identity.generation == identity.generation)
            .ok_or(LoginSessionError::ProfileNotBound)?;
        bound
            .profile
            .profile
            .my_room
            .try_apply_protocol_info(receipt.info())?;
        bound.profile.revision = Some(receipt.revision());
        Ok(())
    }
}

fn ensure_identity_fence(
    expected: &IdentityBinding,
    actual: &IdentityBinding,
) -> Result<(), LoginSessionError> {
    if expected == actual {
        return Ok(());
    }
    Err(LoginSessionError::ProfileIdentityFenceChanged {
        expected_owner: expected.owner,
        expected_user_no: expected.user_no,
        expected_generation: expected.generation,
        actual_owner: actual.owner,
        actual_user_no: actual.user_no,
        actual_generation: actual.generation,
    })
}

fn rider_equipment_write_error(source: RiderEquipmentWriteError) -> LoginSessionError {
    match source {
        RiderEquipmentWriteError::Persistence(RiderEquipmentPersistError::Validation(
            RiderEquipmentValidationError::RiderItemNotGranted { category, item_id },
        )) => LoginSessionError::RiderItemNotGranted { category, item_id },
        RiderEquipmentWriteError::Persistence(RiderEquipmentPersistError::Validation(
            RiderEquipmentValidationError::KartNotGranted { kart_id, serial },
        )) => LoginSessionError::KartNotGranted { kart_id, serial },
        source => LoginSessionError::RiderEquipmentWrite {
            source: Box::new(source),
        },
    }
}

#[derive(Debug)]
struct BoundProfile {
    identity: IdentityBinding,
    profile: ProfileSnapshot,
}

#[derive(Debug, Clone, Copy)]
struct SessionServices<'a> {
    config: &'a ServerConfig,
    world: &'a WorldHandle,
    profiles: &'a ProfileCoordinator,
    session_id: SessionId,
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

async fn read_session_frame<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
    authenticated: bool,
    login_deadline: time::Instant,
    idle_timeout: std::time::Duration,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    if authenticated {
        time::timeout(idle_timeout, read_encrypted_frame(reader, iv, maximum))
            .await
            .map_err(|_| LoginSessionError::SessionIdleTimeout)?
    } else {
        time::timeout_at(login_deadline, read_encrypted_frame(reader, iv, maximum))
            .await
            .map_err(|_| LoginSessionError::LoginTimeout)?
    }
}

async fn write_session_bytes<W>(
    writer: &mut W,
    bytes: &[u8],
    timeout: std::time::Duration,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    time::timeout(timeout, writer.write_all(bytes))
        .await
        .map_err(|_| LoginSessionError::WriteTimeout)??;
    Ok(())
}

pub(crate) async fn run_login_session(
    mut stream: TcpStream,
    peer: SocketAddr,
    config: ServerConfig,
    world: WorldHandle,
    profiles: ProfileCoordinator,
    wire_operations: WireOperationGate,
) -> Result<(), LoginSessionError> {
    let (session_id, mut cancellation, mut outbound) = world
        .register_login_session(peer, wire_operations.clone())
        .await?;
    let registration = SessionRegistration {
        id: session_id,
        world: world.clone(),
        closed: false,
    };
    let services = SessionServices {
        config: &config,
        world: &world,
        profiles: &profiles,
        session_id,
    };
    let result = run_registered_session(
        &mut stream,
        &services,
        &mut cancellation,
        &mut outbound,
        &wire_operations,
    )
    .await;
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

#[allow(
    clippy::too_many_lines,
    reason = "one cancellation boundary must retain the partially-read frame, outbound fairness state, request lease, and writer-only shutdown transition together"
)]
async fn run_registered_session(
    stream: &mut TcpStream,
    services: &SessionServices<'_>,
    cancellation: &mut oneshot::Receiver<()>,
    outbound: &mut mpsc::Receiver<OutboundBatch>,
    wire_operations: &WireOperationGate,
) -> Result<(), LoginSessionError> {
    let config = services.config;
    let peer = peer_label(stream);
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        () = time::sleep(config.first_message_delay) => {}
    }

    // Install the receive state before putting the server-first frame on the
    // wire. No client read begins before this point.
    let mut receive_iv = handshake::initial_iv();
    let mut send_iv = handshake::initial_iv();
    let mut context = SessionContext::default();
    let payload = handshake::first_message_payload()?;
    let wire = frame::encode_plain(&payload, config.max_login_payload)?;
    tokio::select! {
        biased;
        _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
        result = write_session_bytes(stream, &wire, config.session_write_timeout) => result?,
    }

    // Authentication packets may precede PqLogin, but they do not reset this
    // absolute deadline. A client cannot hold a slot forever by trickling
    // harmless pre-login frames.
    let login_deadline = time::Instant::now() + config.login_timeout;
    let (mut reader, mut writer) = stream.split();
    loop {
        // Keep this exact future alive while broadcasts are written. Dropping a
        // partially completed read_exact would consume bytes and desynchronize
        // the next encrypted frame.
        let frame = read_session_frame(
            &mut reader,
            &mut receive_iv,
            config.max_login_payload,
            context.is_authenticated(),
            login_deadline,
            config.session_idle_timeout,
        );
        tokio::pin!(frame);
        let mut outbound_burst = 0;
        let packet = loop {
            let event = tokio::select! {
                biased;
                () = wire_operations.wait_for_request_admission_close() => {
                    return drain_outbound_until_cancelled(
                        &mut writer,
                        cancellation,
                        outbound,
                        &mut send_iv,
                        config,
                    )
                    .await;
                }
                event = select_session_read_event(
                    cancellation,
                    outbound,
                    frame.as_mut(),
                    outbound_burst >= MAX_OUTBOUND_BATCH_BURST,
                ) => event?,
            };
            match event {
                SessionReadEvent::Outbound(batch) => {
                    let batch = batch.ok_or(LoginSessionError::OutboundClosed)?;
                    tokio::select! {
                        biased;
                        _ = &mut *cancellation => {
                            return Err(LoginSessionError::Superseded);
                        }
                        result = write_outbound_batch(
                            &mut writer,
                            batch,
                            &mut send_iv,
                            config,
                        ) => result?,
                    }
                    outbound_burst += 1;
                }
                SessionReadEvent::Frame(result) => break result?,
            }
        };

        let Some(wire_operation) = wire_operations.try_begin_request() else {
            return drain_outbound_until_cancelled(
                &mut writer,
                cancellation,
                outbound,
                &mut send_iv,
                config,
            )
            .await;
        };
        let identity_operation = if context.is_authenticated() {
            Some(tokio::select! {
                biased;
                _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
                result = services.world.admit_identity_operation(services.session_id) => result?,
            })
        } else {
            None
        };
        let operation = SessionFrameOperation::new(wire_operation, identity_operation);
        trace_packet(peer, &packet)?;
        tokio::select! {
            biased;
            _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
            result = process_and_write(
                &mut writer,
                services,
                &packet,
                &mut context,
                &mut send_iv,
                &operation,
            ) => result?,
        }
        // Actor-owned replies are enqueued before their command acknowledgement
        // resolves. Snapshot the bounded FIFO depth at acknowledgement time and
        // flush exactly that prefix while the operation guard remains live.
        // Producers may keep appending behind it, but cannot turn one admitted
        // request into an unbounded drain that blocks migration or shutdown.
        let ready_batches = outbound.len();
        for _ in 0..ready_batches {
            let Ok(batch) = outbound.try_recv() else {
                break;
            };
            tokio::select! {
                biased;
                _ = &mut *cancellation => return Err(LoginSessionError::Superseded),
                result = write_outbound_batch(
                    &mut writer,
                    batch,
                    &mut send_iv,
                    config,
                ) => result?,
            }
        }
        drop(operation);
    }
}

async fn drain_outbound_until_cancelled<W>(
    writer: &mut W,
    cancellation: &mut oneshot::Receiver<()>,
    outbound: &mut mpsc::Receiver<OutboundBatch>,
    send_iv: &mut u32,
    config: &ServerConfig,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    loop {
        tokio::select! {
            biased;
            _ = &mut *cancellation => return Ok(()),
            batch = outbound.recv() => {
                let batch = batch.ok_or(LoginSessionError::OutboundClosed)?;
                tokio::select! {
                    biased;
                    _ = &mut *cancellation => return Ok(()),
                    result = write_outbound_batch(writer, batch, send_iv, config) => result?,
                }
            }
        }
    }
}

async fn process_and_write<W>(
    writer: &mut W,
    services: &SessionServices<'_>,
    packet: &[u8],
    context: &mut SessionContext,
    send_iv: &mut u32,
    operation: &SessionFrameOperation,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    let responses =
        dispatch_packet_admitted(services, packet, context, operation.identity()).await?;
    write_logical_packets(writer, &responses, send_iv, services.config).await
}

async fn write_outbound_batch<W>(
    writer: &mut W,
    batch: OutboundBatch,
    send_iv: &mut u32,
    config: &ServerConfig,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    let (packets, _operation) = batch.into_write_parts();
    write_logical_packets(writer, &packets, send_iv, config).await
}

/// Writes one ordered logical response under one aggregate deadline.
///
/// A batch may contain thousands of small packets. Resetting the timeout for
/// every packet would let a slow-drip peer retain request and outbound
/// operation guards for `packet_count * timeout`.
async fn write_logical_packets<W>(
    writer: &mut W,
    packets: &[Vec<u8>],
    send_iv: &mut u32,
    config: &ServerConfig,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    time::timeout(config.session_write_timeout, async {
        for packet in packets {
            let wire = frame::encode_encrypted(packet, send_iv, config.max_login_payload)?;
            writer.write_all(&wire).await?;
        }
        Ok::<(), LoginSessionError>(())
    })
    .await
    .map_err(|_| LoginSessionError::WriteTimeout)?
}

async fn dispatch_packet_admitted(
    services: &SessionServices<'_>,
    packet: &[u8],
    context: &mut SessionContext,
    operation: Option<&IdentityOperationLease>,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let hash = packet_hash(packet)?;
    if hash == adler32::packet_hash("PqCnAuthenLogin") {
        return Ok(vec![serialize_pr_cn_authen_login()?]);
    }

    if hash == adler32::packet_hash("PqLogin") {
        return handle_login(
            services.config,
            services.world,
            services.profiles,
            services.session_id,
            packet,
            context,
        )
        .await;
    }

    if hash == adler32::packet_hash("PqChannelMovein") {
        return handle_channel_move_in(
            services.config,
            services.world,
            services.profiles,
            services.session_id,
            packet,
            context,
        )
        .await;
    }

    // Every remaining packet is identity-bound. The normal session loop
    // supplies this lease immediately after the global request guard; the
    // fallback preserves a typed unauthenticated error for focused handler
    // tests and defensive in-process callers.
    let late_operation;
    let operation = if let Some(operation) = operation {
        operation
    } else {
        late_operation = services
            .world
            .admit_identity_operation(services.session_id)
            .await?;
        &late_operation
    };
    let world = services.world.admitted(operation);

    if hash == adler32::packet_hash("PqChannelSwitch") {
        return handle_channel_switch(services.config, &world, packet).await;
    }

    if let Some(request) = classify_room_protocol_request(hash) {
        return handle_room_request_admitted(
            &world,
            services.profiles,
            services.session_id,
            request,
            packet,
            context,
        )
        .await;
    }

    if let Some(request) = classify_lobby_request(hash) {
        return handle_lobby_request_admitted(&world, request, packet).await;
    }

    if let Some(request) = classify_race_request(hash) {
        return handle_race_request_admitted(&world, request, packet).await;
    }

    if let Some(request) = classify_myroom_request(hash) {
        return handle_myroom_request(
            &world,
            services.profiles,
            services.session_id,
            request,
            packet,
            context,
        )
        .await;
    }

    if let Some(request) = classify_equipment_request(hash) {
        return dispatch_equipment_request(&world, services.profiles, request, packet, context)
            .await;
    }

    if let Some(request) = classify_startup_request(hash) {
        return handle_startup_request(
            &world,
            services.profiles,
            services.session_id,
            request,
            packet,
            context,
        )
        .await;
    }

    if is_startup_noop(hash) {
        let identity = world.authorize_identity().await?;
        let _ = context.profile_for(&identity)?;
        return Ok(Vec::new());
    }

    // Identity-bound packets cannot be processed by a stale connection. Their
    // concrete handlers are ported incrementally on top of this fence.
    let _ = world.authorize_identity().await?;
    Ok(Vec::new())
}

async fn handle_channel_switch(
    config: &ServerConfig,
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let request = parse_pq_channel_switch(packet)?;
    let selected_channel =
        resolve_channel_id(request.requested_game_type, request.preferred_channel_id).ok_or(
            LoginSessionError::UnsupportedChannel {
                game_type: request.requested_game_type,
                preferred_channel: request.preferred_channel_id,
            },
        )?;
    let permit = world
        .begin_migration(
            ChannelBinding {
                channel_id: selected_channel,
                game_type: request.requested_game_type,
            },
            random_migration_token(),
            Instant::now(),
        )
        .await?;
    Ok(vec![serialize_pr_channel_switch(
        selected_channel,
        permit.token.get(),
        config.advertised_address,
        config.ports.login_tcp(),
    )])
}

async fn handle_login(
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let login = parse_pq_login(packet)?;
    let admission = profiles.admit(&login.nickname, "login profile").await?;
    let claimed = world.claim_identity(session_id, login.nickname).await?;
    let (profile, lane) = profiles
        .load(
            claimed.nickname.clone(),
            config.allow_remote_profile_creation || claimed.source_ip.is_loopback(),
            admission,
        )
        .await?;
    let identity = world.authorize_identity(session_id).await?;
    ensure_identity_fence(&claimed, &identity)?;
    context.bind_profile(identity.clone(), profile);
    let profile = context.profile_for(&identity)?;

    let response = serialize_pr_login(&PrLoginFields {
        time: current_legacy_time(),
        user_no: identity.user_no.get(),
        nickname: identity.nickname,
        pmap: profile.rider.pmap,
        advertised_address: config.advertised_address,
        game_udp_port: config.ports.game_udp(),
        p2p_udp_port: config.ports.p2p_udp(),
        screen: profile.game_option.screen,
    })?;
    drop(lane);
    Ok(vec![response])
}

async fn handle_channel_move_in(
    config: &ServerConfig,
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let request = parse_pq_channel_movein(packet)?;
    let user_no = UserNo::new(request.user_no).ok_or(LoginSessionError::InvalidUserNo)?;
    let token = MigrationToken::new(request.migration_token)
        .ok_or(LoginSessionError::InvalidMigrationToken)?;

    let preflight = world
        .preflight_migration(
            session_id,
            user_no,
            request.channel_id,
            token,
            Instant::now(),
        )
        .await?;
    preflight.wait_for_operations_drained().await?;
    let admission = profiles
        .admit(preflight.nickname(), "load migrated profile")
        .await?;
    let (profile, lane) = profiles
        .load(
            preflight.nickname().to_owned(),
            config.allow_remote_profile_creation || preflight.destination_ip().is_loopback(),
            admission,
        )
        .await?;
    let presentation = myroom_profile_presentation(&profile.profile);
    let profile_lease = MyRoomProfileLease::new(presentation, lane);
    let acknowledgement =
        serialize_pr_channel_move_in(config.ports.game_udp(), config.ports.p2p_udp());
    let completion = world
        .complete_preflighted_migration_with_acknowledgement(
            preflight,
            profile_lease,
            acknowledgement,
        )
        .await?;
    let identity = world.authorize_identity(session_id).await?;
    ensure_identity_fence(&completion.binding, &identity)?;
    context.bind_profile(identity, profile);

    Ok(Vec::new())
}

async fn handle_room_request_admitted(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    _session_id: SessionId,
    request: RoomProtocolRequest,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let payload = match request {
        RoomProtocolRequest::RoomList => {
            RoomCommandPayload::List(parse_ch_get_room_list_request(packet)?)
        }
        RoomProtocolRequest::CreateRoom => {
            let request = parse_ch_create_room_request(packet)?;
            let identity = world.authorize_identity().await?;
            let profile = context.profile_for(&identity)?;
            RoomCommandPayload::Create {
                request,
                participant: room_participant_from_profile(&identity, profile, profiles.catalog())?,
            }
        }
        RoomProtocolRequest::JoinRoom => {
            let request = parse_ch_join_room_request(packet)?;
            let identity = world.authorize_identity().await?;
            let profile = context.profile_for(&identity)?;
            RoomCommandPayload::Join {
                request,
                participant: room_participant_from_profile(&identity, profile, profiles.catalog())?,
            }
        }
        RoomProtocolRequest::LeaveRoom => {
            let _ = parse_ch_leave_room_request(packet)?;
            RoomCommandPayload::Leave
        }
        RoomProtocolRequest::FirstRoomState => {
            parse_gr_first_request(packet)?;
            RoomCommandPayload::FirstState
        }
    };
    world.room_protocol(payload).await?;
    Ok(Vec::new())
}

async fn handle_lobby_request_admitted(
    world: &AdmittedWorldHandle<'_>,
    request: LobbyRequest,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let payload = match request {
        LobbyRequest::SetSlotState => {
            LobbyCommandPayload::SetSlotState(parse_set_slot_state_request(packet)?.state)
        }
        LobbyRequest::ChangeTeam => {
            LobbyCommandPayload::ChangeTeam(parse_change_team_request(packet)?.team)
        }
        LobbyRequest::ChangeMaster => {
            LobbyCommandPayload::ChangeMaster(parse_change_master_request(packet)?.target_nickname)
        }
        LobbyRequest::StartRoom => {
            let _ = parse_start_room_request(packet)?;
            LobbyCommandPayload::StartRoom(StartRoomPlan::new(
                vec![P5136_FALLBACK_TRACK_ID],
                Vec::new(),
            ))
        }
    };
    match world.lobby_command(payload).await {
        Ok(_) => {}
        Err(WorldError::Lobby(
            error @ (LobbyError::NotInRoom
            | LobbyError::NotLobby { .. }
            | LobbyError::HumanRacerRequired
            | LobbyError::ObserverStateServerOwned
            | LobbyError::PreparingStateServerOwned
            | LobbyError::NotRoomMaster
            | LobbyError::InvalidMasterTarget { .. }
            | LobbyError::TeamModeRequired
            | LobbyError::TeamFull { .. }
            | LobbyError::NoRacers
            | LobbyError::AiParticipantsUnsupported
            | LobbyError::RacerNotReady { .. }
            | LobbyError::MissingTrackCandidates
            | LobbyError::WorldQuiescing),
        )) => {
            tracing::debug!(
                %error,
                session_id = world.session_id().get(),
                "rejected a lobby command without terminating the session"
            );
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Vec::new())
}

async fn handle_race_request_admitted(
    world: &AdmittedWorldHandle<'_>,
    request: RaceRequest,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let payload = match request {
        RaceRequest::GameControl => {
            RaceCommandPayload::GameControl(parse_game_control_request(packet)?)
        }
        RaceRequest::AiGoalIn => RaceCommandPayload::AiGoalIn(parse_ai_goal_in_request(packet)?),
        RaceRequest::TeamBoosterGauge => {
            RaceCommandPayload::TeamBoosterGauge(parse_team_booster_request(packet)?)
        }
    };
    match world.race_command(payload).await {
        Ok(_) => {}
        Err(WorldError::Race(error)) if error.is_expected_rejection() => {
            tracing::debug!(
                %error,
                session_id = world.session_id().get(),
                "rejected a race command without terminating the session"
            );
        }
        Err(error) => return Err(error.into()),
    }
    Ok(Vec::new())
}

async fn handle_myroom_request(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: MyRoomRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    match request {
        MyRoomRequest::FirstState => {
            parse_first_request(packet)?;
            execute_live_myroom_command(
                world,
                profiles,
                session_id,
                MyRoomCommandPayload::FirstState,
                context,
            )
            .await?;
            return Ok(Vec::new());
        }
        MyRoomRequest::RequestItems => {
            parse_request_items(packet)?;
            execute_myroom_owner_items(world, profiles, session_id, context).await?;
            return Ok(Vec::new());
        }
        MyRoomRequest::Secede => {
            parse_secede_request(packet)?;
            execute_live_myroom_command(
                world,
                profiles,
                session_id,
                MyRoomCommandPayload::Secede,
                context,
            )
            .await?;
            return Ok(Vec::new());
        }
        MyRoomRequest::UpdateInfo => {}
        _ => {
            let identity = world.authorize_identity().await?;
            let _ = context.profile_for(&identity)?;
            return Ok(Vec::new());
        }
    }

    let Some(view) = world.myroom_session_view().await? else {
        return Ok(Vec::new());
    };
    if view.role() == MyRoomSessionRole::Visitor {
        return Ok(vec![serialize_myroom_info(view.info())?]);
    }

    let proposed = parse_update_info(packet)?;
    let before = world.authorize_identity().await?;
    let _ = context.profile_for(&before)?;
    let admission = profiles
        .admit_for_operation(
            world.operation(),
            &before.nickname,
            MYROOM_INFO_WRITE_OPERATION,
        )
        .await?;
    let receipt = world
        .persist_myroom_owner_info(proposed, admission)
        .await
        .map_err(myroom_info_write_error)?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    context.apply_myroom_info_write(&after, &receipt)?;
    tracing::trace!(
        nickname = %after.nickname,
        revision = receipt.revision(),
        publication = ?receipt.publication(),
        "applied durable MyRoom owner info to the bound session profile"
    );
    Ok(Vec::new())
}

async fn execute_myroom_owner_items(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    context: &SessionContext,
) -> Result<(), LoginSessionError> {
    for attempt in 0..MAX_MYROOM_WIRE_PLAN_ATTEMPTS {
        let plan = world.prepare_myroom_owner_items().await?;
        let _ = context.profile_for(plan.expected_identity())?;
        let loaded = if plan.owner_items_visible() {
            let owner = plan
                .owner_identity()
                .ok_or(WorldError::MyRoomOwnerItemPlanMismatch {
                    session: session_id,
                })?
                .clone();
            let owner_nickname = owner.nickname.clone();
            let admission = profiles
                .admit_for_operation(
                    world.operation(),
                    &owner_nickname,
                    MYROOM_OWNER_ITEM_READ_OPERATION,
                )
                .await?;
            let (batch, lane) = profiles
                .load_myroom_owner_items(owner_nickname, admission)
                .await?;
            Some(MyRoomOwnerItemLoad::new(owner, batch.into_packets(), lane))
        } else {
            None
        };
        let prepared = plan.complete(loaded)?;
        match world.publish_myroom_owner_items(prepared).await {
            Err(WorldError::MyRoomWirePlanStale { .. })
                if attempt + 1 < MAX_MYROOM_WIRE_PLAN_ATTEMPTS =>
            {
                tracing::trace!(
                    session_id = session_id.get(),
                    attempt = attempt + 1,
                    "retrying MyRoom owner items after authorization changed during profile I/O"
                );
            }
            result => return Ok(result?),
        }
    }
    unreachable!("the bounded MyRoom owner-item plan loop always returns on its final attempt")
}

async fn execute_live_myroom_command(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    payload: MyRoomCommandPayload,
    context: &SessionContext,
) -> Result<(), LoginSessionError> {
    for attempt in 0..MAX_MYROOM_WIRE_PLAN_ATTEMPTS {
        let plan = world.prepare_myroom_command().await?;
        let _ = context.profile_for(plan.expected_identity())?;
        let projection = match plan.wire_plan() {
            Some(wire) => Some(
                profiles
                    .load_myroom_wire_projection_for_operation(world.operation(), wire)
                    .await?,
            ),
            None => None,
        };
        let prepared = plan.complete(projection)?;
        match world.myroom_command(payload, prepared).await {
            Err(WorldError::MyRoomWirePlanStale { .. })
                if attempt + 1 < MAX_MYROOM_WIRE_PLAN_ATTEMPTS =>
            {
                tracing::trace!(
                    session_id = session_id.get(),
                    attempt = attempt + 1,
                    "retrying MyRoom command after live roster changed during profile I/O"
                );
            }
            result => return Ok(result?),
        }
    }
    unreachable!("the bounded MyRoom wire-plan loop always returns on its final attempt")
}

fn myroom_info_write_error(source: MyRoomInfoWriteError) -> LoginSessionError {
    LoginSessionError::MyRoomInfoWrite {
        source: Box::new(source),
    }
}

async fn dispatch_equipment_request(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    request: EquipmentRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    handle_equipment_request_admitted(
        world,
        profiles,
        world.session_id(),
        request,
        packet,
        context,
    )
    .await
}

async fn handle_equipment_request_admitted(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: EquipmentRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    match request {
        EquipmentRequest::SetRiderItems => {
            if packet.len() < size_of::<u32>() + RIDER_ITEM_SNAPSHOT_WIRE_LENGTH {
                let identity = world.authorize_identity().await?;
                let _ = context.profile_for(&identity)?;
                tracing::debug!(
                    body_length = packet.len().saturating_sub(size_of::<u32>()),
                    required = RIDER_ITEM_SNAPSHOT_WIRE_LENGTH,
                    "ignored truncated P5136 rider-equipment request"
                );
                return Ok(Vec::new());
            }
            let selection = parse_set_rider_items(packet)?;
            update_rider_equipment(world, profiles, session_id, selection, context).await?;
            Ok(Vec::new())
        }
        EquipmentRequest::EquipPlantPart => {
            let request = match parse_equip_plant_part(packet) {
                Ok(request) => request,
                Err(error) => {
                    tracing::debug!(%error, "rejected malformed P5136 plant-part request");
                    let identity = world.authorize_identity().await?;
                    let _ = context.profile_for(&identity)?;
                    return Ok(vec![serialize_equip_tuning_failure()]);
                }
            };
            equip_plant_part(world, profiles, session_id, request, context).await
        }
    }
}

async fn update_rider_equipment(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    _session_id: SessionId,
    selection: RiderItemSelection,
    context: &mut SessionContext,
) -> Result<(), LoginSessionError> {
    let before = world.authorize_identity().await?;
    let _ = context.profile_for(&before)?;
    let admission = profiles
        .admit_for_operation(
            world.operation(),
            &before.nickname,
            "update rider equipment",
        )
        .await?;
    let completion = world
        .reserve_rider_equipment_completion()
        .await
        .map_err(rider_equipment_write_error)?;
    let prepared = profiles.prepare_rider_equipment_write(selection, admission, completion)?;
    let receipt = world
        .persist_rider_equipment(prepared)
        .await
        .map_err(rider_equipment_write_error)?;
    tracing::trace!(
        nickname = %before.nickname,
        revision = receipt.revision(),
        publication = ?receipt.publication(),
        "reloading the full bound profile after a durable rider-equipment write"
    );
    let reload_admission = profiles
        .admit_for_operation(
            world.operation(),
            &before.nickname,
            "reload profile after rider equipment",
        )
        .await?;
    let (profile, lane) = profiles
        .load(before.nickname.clone(), false, reload_admission)
        .await?;
    if profile
        .revision
        .is_none_or(|revision| revision < receipt.revision())
    {
        let actual = profile.revision;
        drop(lane);
        return Err(LoginSessionError::RiderEquipmentReloadBehind {
            nickname: before.nickname,
            durable: receipt.revision(),
            actual,
        });
    }
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    context.bind_profile(after, profile);
    drop(lane);
    Ok(())
}

async fn equip_plant_part(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    _session_id: SessionId,
    request: PlantPartEquipRequest,
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let nickname = context.bound_identity()?.nickname.clone();
    let admission = profiles
        .admit_for_operation(world.operation(), &nickname, "equip plant part")
        .await?;
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    let (equipped, lane) = match profiles.equip_plant_part(request, admission).await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, "failed to persist P5136 plant-part selection");
            return Ok(vec![serialize_equip_tuning_failure()]);
        }
    };
    let after_write = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after_write)?;
    let _ = context.profile_for(&after_write)?;
    drop(lane);
    Ok(vec![if equipped {
        serialize_equip_tuning_success(request)
    } else {
        serialize_equip_tuning_failure()
    }])
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoomKartBaseResolution {
    KartZeroBaseline,
    CatalogBaseSpec,
    MissingCatalogFallback,
    MissingCatalogSpecFallback,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RoomPhysicsFallbackReason {
    CatalogUnavailable,
    CatalogSpecUnavailable,
    FlyingPetNotApplied { item_id: u16 },
    KartPlantNotApplied { slot: u8, item_id: u16 },
    SpeedPatchNotApplied { value: u8 },
    TuneLevelV2SidecarsUninspected,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RoomPhysicsMetadata {
    kart_id: u16,
    base_resolution: RoomKartBaseResolution,
    fallback_reasons: Vec<RoomPhysicsFallbackReason>,
    block: P5136KartPhysicsBlock,
}

impl RoomPhysicsMetadata {
    fn physics_fallback(&self) -> bool {
        !self.fallback_reasons.is_empty()
    }
}

fn room_physics_metadata(
    profile: &Profile,
    catalog: Option<&CatalogInventory>,
) -> Result<RoomPhysicsMetadata, KartPhysicsBuildError> {
    let items = &profile.rider_item;
    let mut snapshot = P5136KartPhysicsSnapshot::csharp_s7_baseline();
    let mut fallback_reasons = Vec::new();
    let base_resolution = if items.kart == 0 {
        RoomKartBaseResolution::KartZeroBaseline
    } else if let Some(catalog) = catalog {
        if let Some(spec) = catalog.kart_spec(items.kart) {
            snapshot.kart = *spec;
            RoomKartBaseResolution::CatalogBaseSpec
        } else {
            fallback_reasons.push(RoomPhysicsFallbackReason::CatalogSpecUnavailable);
            RoomKartBaseResolution::MissingCatalogSpecFallback
        }
    } else {
        fallback_reasons.push(RoomPhysicsFallbackReason::CatalogUnavailable);
        RoomKartBaseResolution::MissingCatalogFallback
    };

    if items.flying_pet != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::FlyingPetNotApplied {
            item_id: items.flying_pet,
        });
    }
    for (slot, item_id) in [
        items.kart_plant1,
        items.kart_plant2,
        items.kart_plant3,
        items.kart_plant4,
    ]
    .into_iter()
    .enumerate()
    .filter(|(_, item_id)| *item_id != 0)
    {
        fallback_reasons.push(RoomPhysicsFallbackReason::KartPlantNotApplied {
            slot: u8::try_from(slot + 1).expect("the four kart-plant slots fit in u8"),
            item_id,
        });
    }
    if profile.server_setting.speed_patch_use != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::SpeedPatchNotApplied {
            value: profile.server_setting.speed_patch_use,
        });
    }
    if items.kart != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected);
    }

    Ok(RoomPhysicsMetadata {
        kart_id: items.kart,
        base_resolution,
        fallback_reasons,
        block: build_p5136_kart_physics_block(&snapshot)?,
    })
}

fn room_participant_from_profile(
    identity: &IdentityBinding,
    profile: &Profile,
    catalog: Option<&CatalogInventory>,
) -> Result<RoomParticipant, LoginSessionError> {
    let physics = room_physics_metadata(profile, catalog)?;
    if physics.physics_fallback() {
        tracing::warn!(
            nickname = %identity.nickname,
            kart_id = physics.kart_id,
            base_resolution = ?physics.base_resolution,
            physics_fallback = true,
            fallback_reasons = ?physics.fallback_reasons,
            "using bounded room-entry kart physics with omitted optional contributions"
        );
    }
    let observer = matches!(profile.rider.pmap, 590 | 718);
    let (p2p_address, p2p_port) = profile_p2p_endpoint(identity.source_ip, profile);
    let club_name = if profile.rider.club_mark_logo == 0 {
        String::new()
    } else {
        profile.rider.club_name.clone()
    };
    Ok(RoomParticipant {
        player: RoomPlayer {
            player_type: if observer { 4 } else { 2 },
            user_no: identity.user_no.get(),
            p2p_address,
            p2p_port,
            nickname: identity.nickname.clone(),
            emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
            emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
            rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
            card: profile.rider.card.clone(),
            rp: profile.rider.rp,
            team: 0,
            ranking: 0,
            rider_school_level: 0,
            club_name,
            club_mark_logo: profile.rider.club_mark_logo,
        },
        observer,
        kart_physics: physics.block,
    })
}

fn profile_p2p_endpoint(source_ip: IpAddr, profile: &Profile) -> (Ipv4Addr, u16) {
    let address = match source_ip {
        IpAddr::V4(address) => address,
        IpAddr::V6(_) => Ipv4Addr::UNSPECIFIED,
    };
    let port = u16::try_from(profile.rider.p2p_port).unwrap_or_default();
    (address, port)
}

/// Builds the profile-owned portion of a `MyRoom` player slot and binds its
/// identity fields to one exact World generation.
///
/// Callers must still reauthorize that binding in the World actor after any
/// profile I/O. The actor may overwrite `user_no` and `nickname` from its
/// authoritative binding before committing a hub transition.
#[cfg(test)]
fn myroom_player_slot_from_profile(
    identity: &IdentityBinding,
    profile: &Profile,
) -> MyRoomPlayerSlot {
    myroom_profile_presentation(profile).player_for(identity)
}

async fn handle_startup_request(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: StartupRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    if request == StartupRequest::GetRider {
        return handle_get_rider_admitted(world, profiles, session_id, context).await;
    }
    if request == StartupRequest::UpdateGameOption {
        update_game_options_admitted(world, profiles, session_id, packet, context).await?;
        return Ok(Vec::new());
    }

    let identity = world.authorize_identity().await?;
    let profile = context.profile_for(&identity)?;
    Ok(startup_response(request, profile).into_iter().collect())
}

async fn handle_get_rider_admitted(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    _session_id: SessionId,
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let nickname = context.bound_identity()?.nickname.clone();
    let admission = profiles
        .admit_for_operation(world.operation(), &nickname, "load fresh rider inventory")
        .await?;
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    let (responses, profile, lane) = profiles
        .get_rider_sequence(before.nickname.clone(), admission)
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    let _ = context.profile_for(&after)?;
    world
        .refresh_myroom_presentation(
            after.clone(),
            MyRoomProfileLease::new(myroom_profile_presentation(&profile.profile), lane),
        )
        .await?;
    context.bind_profile(after, profile);
    Ok(responses)
}

async fn update_game_options_admitted(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    _session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<(), LoginSessionError> {
    let request = parse_pq_update_game_option(packet)?;
    let nickname = context.bound_identity()?.nickname.clone();
    let admission = profiles
        .admit_for_operation(world.operation(), &nickname, "update game options")
        .await?;
    let before = world.authorize_identity().await?;
    let _ = context.profile_for(&before)?;
    let (profile, lane) = profiles
        .update_game_options(before.nickname.clone(), request.options, admission)
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    let _ = context.profile_for(&after)?;
    context.bind_profile(after, profile);
    drop(lane);
    Ok(())
}

fn profile_rider_fields(nickname: String, profile: &Profile) -> PrGetRiderFields {
    PrGetRiderFields {
        nickname,
        emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
        emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
        rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
        lucci: profile.rider.lucci,
        rp: i32::from_le_bytes(profile.rider.rp.to_le_bytes()),
    }
}

fn startup_response(request: StartupRequest, profile: &Profile) -> Option<Vec<u8>> {
    let time = current_legacy_time();
    Some(match request {
        StartupRequest::LoginVipInfo => startup::serialize_pr_login_vip_info(profile.rider.premium),
        StartupRequest::EventReward => startup::serialize_lo_rp_event_reward(),
        StartupRequest::AddRacingTime => startup::serialize_lo_rp_add_racing_time(),
        StartupRequest::EquipTuning => startup::serialize_pr_equip_tuning_failure(),
        StartupRequest::GetGameOption => {
            startup::serialize_pr_get_game_option(&profile_game_options(&profile.game_option))
        }
        StartupRequest::SetPlaytimeEventTick => startup::serialize_pr_set_playtime_event_tick(),
        StartupRequest::ChapterInfo => startup::serialize_pr_chapter_info(),
        StartupRequest::GetDuelMissionBulk => startup::serialize_pr_get_duel_mission_bulk(time),
        StartupRequest::RiderSchoolData => startup::serialize_pr_rider_school_data(time),
        StartupRequest::RiderSchoolProgress => startup::serialize_pr_rider_school_progress(),
        StartupRequest::ChannelStatic => startup::serialize_channel_static_reply(),
        StartupRequest::DynamicCommand => startup::serialize_pr_dynamic_command(),
        StartupRequest::PublicCommand => startup::serialize_pr_public_command(),
        StartupRequest::GetFavoriteChannel => startup::serialize_pr_get_favorite_channel(),
        StartupRequest::KartPassInit => startup::serialize_pr_kart_pass_init(),
        StartupRequest::KartPassReward => startup::serialize_pr_kart_pass_reward(),
        StartupRequest::QuestUxSecond => startup::serialize_pr_quest_ux_second(),
        StartupRequest::GetCurrentRider => startup::serialize_pr_get_current_rider(),
        StartupRequest::DisassembleFeeInfo => startup::serialize_pr_disassemble_fee_info(),
        StartupRequest::SyncDictionaryInfo => startup::serialize_pr_sync_dictionary_info(),
        StartupRequest::AddTimeEventInit => startup::serialize_pr_add_time_event_init(time),
        StartupRequest::GetRider | StartupRequest::UpdateGameOption => return None,
    })
}

fn profile_game_options(options: &p5136_profile::GameOptions) -> startup::GameOptions {
    startup::GameOptions {
        bgm_volume: options.bgm_volume,
        sound_volume: options.sound_volume,
        main_bgm: options.main_bgm,
        sound_effect: options.sound_effect,
        full_screen: options.full_screen,
        show_mirror: options.show_mirror,
        show_other_player_names: options.show_other_player_names,
        show_outlines: options.show_outlines,
        show_shadows: options.show_shadows,
        high_level_effect: options.high_level_effect,
        motion_blur_effect: options.motion_blur_effect,
        motion_distortion_effect: options.motion_distortion_effect,
        high_end_optimization: options.high_end_optimization,
        auto_ready: options.auto_ready,
        prop_description: options.prop_description,
        video_quality: options.video_quality,
        bgm_check: options.bgm_check,
        sound_check: options.sound_check,
        show_hit_info: options.show_hit_info,
        auto_boost: options.auto_boost,
        game_type: options.game_type,
        set_ghost: options.set_ghost,
        speed_type: options.speed_type,
        room_chat: options.room_chat,
        driving_chat: options.driving_chat,
        show_all_player_hit_info: options.show_all_player_hit_info,
        show_team_color: options.show_team_color,
        set_screen: options.screen,
        hide_competitive_rank: options.hide_competitive_rank,
    }
}

fn apply_game_options(destination: &mut p5136_profile::GameOptions, source: &startup::GameOptions) {
    destination.bgm_volume = source.bgm_volume;
    destination.sound_volume = source.sound_volume;
    destination.main_bgm = source.main_bgm;
    destination.sound_effect = source.sound_effect;
    destination.full_screen = source.full_screen;
    destination.show_mirror = source.show_mirror;
    destination.show_other_player_names = source.show_other_player_names;
    destination.show_outlines = source.show_outlines;
    destination.show_shadows = source.show_shadows;
    destination.high_level_effect = source.high_level_effect;
    destination.motion_blur_effect = source.motion_blur_effect;
    destination.motion_distortion_effect = source.motion_distortion_effect;
    destination.high_end_optimization = source.high_end_optimization;
    destination.auto_ready = source.auto_ready;
    destination.prop_description = source.prop_description;
    destination.video_quality = source.video_quality;
    destination.bgm_check = source.bgm_check;
    destination.sound_check = source.sound_check;
    destination.show_hit_info = source.show_hit_info;
    destination.auto_boost = source.auto_boost;
    destination.game_type = source.game_type;
    destination.set_ghost = source.set_ghost;
    destination.speed_type = source.speed_type;
    destination.room_chat = source.room_chat;
    destination.driving_chat = source.driving_chat;
    destination.show_all_player_hit_info = source.show_all_player_hit_info;
    destination.show_team_color = source.show_team_color;
    destination.screen = source.set_screen;
    destination.hide_competitive_rank = source.hide_competitive_rank;
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
async fn dispatch_packet(
    services: &SessionServices<'_>,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    dispatch_packet_admitted(services, packet, context, None).await
}

#[cfg(test)]
async fn handle_room_request(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: RoomProtocolRequest,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Focused parser tests intentionally use an unauthenticated sentinel
    // session. Preserve the real handler's parse-before-mutation contract
    // without letting the test-only admission adapter mask a wire error.
    match request {
        RoomProtocolRequest::RoomList => {
            let _ = parse_ch_get_room_list_request(packet)?;
        }
        RoomProtocolRequest::CreateRoom => {
            let _ = parse_ch_create_room_request(packet)?;
        }
        RoomProtocolRequest::JoinRoom => {
            let _ = parse_ch_join_room_request(packet)?;
        }
        RoomProtocolRequest::LeaveRoom => {
            let _ = parse_ch_leave_room_request(packet)?;
        }
        RoomProtocolRequest::FirstRoomState => {
            parse_gr_first_request(packet)?;
        }
    }
    let operation = world.admit_identity_operation(session_id).await?;
    handle_room_request_admitted(
        &world.admitted(&operation),
        profiles,
        session_id,
        request,
        packet,
        context,
    )
    .await
}

#[cfg(test)]
async fn handle_lobby_request(
    world: &WorldHandle,
    session_id: SessionId,
    request: LobbyRequest,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let operation = world.admit_identity_operation(session_id).await?;
    handle_lobby_request_admitted(&world.admitted(&operation), request, packet).await
}

#[cfg(test)]
async fn handle_race_request(
    world: &WorldHandle,
    session_id: SessionId,
    request: RaceRequest,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let operation = world.admit_identity_operation(session_id).await?;
    handle_race_request_admitted(&world.admitted(&operation), request, packet).await
}

#[cfg(test)]
async fn handle_equipment_request(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    request: EquipmentRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let operation = world.admit_identity_operation(session_id).await?;
    handle_equipment_request_admitted(
        &world.admitted(&operation),
        profiles,
        session_id,
        request,
        packet,
        context,
    )
    .await
}

#[cfg(test)]
async fn handle_get_rider(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let operation = world.admit_identity_operation(session_id).await?;
    handle_get_rider_admitted(&world.admitted(&operation), profiles, session_id, context).await
}

#[cfg(test)]
async fn update_game_options(
    world: &WorldHandle,
    profiles: &ProfileCoordinator,
    session_id: SessionId,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<(), LoginSessionError> {
    let operation = world.admit_identity_operation(session_id).await?;
    update_game_options_admitted(
        &world.admitted(&operation),
        profiles,
        session_id,
        packet,
        context,
    )
    .await
}

#[cfg(test)]
mod tests {
    use std::{
        array,
        fmt::Write as _,
        fs,
        future::Future,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
        time::{Duration, Instant},
    };

    use p5136_core::{
        adler32,
        channel::serialize_pr_channel_move_in,
        equipment_protocol::{
            EquipmentRequest, PlantPartEquipRequest, RiderItemSelection,
            serialize_equip_tuning_failure, serialize_equip_tuning_success,
        },
        frame::{DEFAULT_MAX_PAYLOAD, encode_encrypted},
        kart_physics::{P5136KartPhysicsSnapshot, build_p5136_kart_physics_block},
        lobby_protocol::{LobbyProtocolError, LobbyRequest, PlayerSlotState},
        myroom_protocol::{
            MAX_MYROOM_PASSWORD_UTF16_UNITS, MYROOM_SLOT_COUNT, MyRoomInfo, MyRoomKart,
            MyRoomParts, MyRoomProtocolError, MyRoomSlot, MyRoomTune, OWNER_ITEM_NAME,
            REQUEST_MYROOM_ITEMS_NAME, plan_owner_item_packets, serialize_missing_owner_items,
            serialize_myroom_info, serialize_owner_item_enchants, serialize_owner_items,
            serialize_secede_reply, serialize_slot_data,
        },
        packet::{PacketReader, PacketWriter},
        race_protocol::{
            GameControlRequest, RaceProtocolError, RaceRequest, parse_game_control_request,
        },
        room_protocol::{
            ChCreateRoomRequest, ChJoinRoomRequest, MAX_CLUB_NAME_UTF16_UNITS,
            ROOM_CONNECTION_CONTEXT_LENGTH, ROOM_DATA_LENGTH, RoomProtocolError,
            RoomProtocolRequest,
        },
        startup::{GameOptions, RIDER_ITEM_SNAPSHOT_WIRE_LENGTH},
    };
    use p5136_profile::{
        CatalogInventory, EquipmentExceptions, GrantedKart, MyRoomItemStateError, Profile,
        ProfileStore, rider_item_snapshot,
    };
    use tokio::io::{AsyncWrite, AsyncWriteExt, duplex};
    use tokio::sync::{mpsc, oneshot};
    use tokio::time;

    use super::{
        BlockingUpdateHook, LoginSessionError, MAX_MYROOM_ITEM_RECORDS,
        MAX_MYROOM_OWNER_ITEM_BYTES, MAX_MYROOM_OWNER_ITEM_PACKETS, MAX_OUTBOUND_BATCH_BURST,
        MyRoomOwnerItemPacketBatch, ProfileCoordinator, RiderEquipmentValidationError,
        RoomKartBaseResolution, RoomPhysicsFallbackReason, SessionContext, SessionReadEvent,
        SessionServices, dispatch_packet, handle_channel_move_in, handle_equipment_request,
        handle_get_rider, handle_lobby_request, handle_race_request, handle_room_request,
        myroom_player_slot_from_profile, myroom_profile_presentation, read_encrypted_frame,
        read_session_frame, room_participant_from_profile, room_physics_metadata,
        select_session_read_event, update_game_options, write_outbound_batch, write_session_bytes,
    };
    use crate::equipment_persistence::validate_rider_item_selection;
    use crate::operation_gate::WireOperationGate;
    use crate::profile_io::MyRoomProfileLease;
    use crate::{
        ChannelBinding, IdentityBinding, IdentityError, MigrationToken, ServerConfig, SessionId,
        WorldError, WorldHandle,
        world::test_support::{spawn_myroom_world, spawn_myroom_world_with_outbound_capacity},
        world::{OutboundBatch, RaceCommandOutcome, RaceCommandPayload, RoomCommandPayload},
    };

    struct DelayedPacketWriter {
        delay: Duration,
        pending: Option<Pin<Box<time::Sleep>>>,
    }

    impl DelayedPacketWriter {
        fn new(delay: Duration) -> Self {
            Self {
                delay,
                pending: None,
            }
        }
    }

    impl AsyncWrite for DelayedPacketWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            let this = self.get_mut();
            let sleep = this
                .pending
                .get_or_insert_with(|| Box::pin(time::sleep(this.delay)));
            match sleep.as_mut().poll(cx) {
                Poll::Pending => Poll::Pending,
                Poll::Ready(()) => {
                    this.pending = None;
                    Poll::Ready(Ok(buffer.len()))
                }
            }
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn test_catalog() -> Arc<CatalogInventory> {
        const GRANT_CATEGORIES: &[u16] = &[
            1, 2, 3, 4, 7, 8, 9, 11, 12, 13, 14, 16, 18, 20, 21, 22, 23, 26, 27, 28, 30, 31, 32,
            36, 37, 38, 39, 43, 44, 45, 46, 49, 52, 53, 55, 59, 61, 67, 68, 69, 70,
        ];
        const NON_GRANT_CATEGORIES: &[u16] = &[
            5, 6, 10, 15, 17, 19, 24, 25, 29, 33, 34, 35, 40, 41, 42, 47, 48, 50, 51,
        ];

        let mut items = String::new();
        let mut item_count = 0;
        for &category in GRANT_CATEGORIES {
            let ids: Box<dyn Iterator<Item = u16>> = if category == 3 {
                Box::new((1..=1_198).chain([1_450, 1_453]))
            } else {
                Box::new(1_000..1_110)
            };
            for id in ids {
                writeln!(
                    items,
                    r#"<Item category="{category}" id="{id}" name="test" />"#
                )
                .unwrap();
                item_count += 1;
            }
        }
        for (index, &category) in NON_GRANT_CATEGORIES.iter().enumerate() {
            let count = 63 + usize::from(index < 3);
            for id in 1..=count {
                writeln!(
                    items,
                    r#"<Item category="{category}" id="{id}" name="test" />"#
                )
                .unwrap();
                item_count += 1;
            }
        }
        assert_eq!(item_count, 6_800);
        let xml = format!(
            r#"<KartCatalog formatVersion="3" protocolVersion="5136" region="kr">
                <Names>
                    <Kart id="1450" name="sessionKnownKart" />
                    <Kart id="1453" name="sessionMissingKartSpec" />
                </Names>
                <Specs>
                    <Spec name="sessionKnownKart">
                        <BodyParam ForwardAccelForce="147" DragFactor="-0.05" />
                    </Spec>
                </Specs>
                <Inventory total="{item_count}" categories="60">{items}</Inventory>
            </KartCatalog>"#
        );
        Arc::new(CatalogInventory::from_xml(xml.as_bytes()).unwrap())
    }

    async fn bind_test_profile(
        profiles: &ProfileCoordinator,
        identity: &IdentityBinding,
    ) -> SessionContext {
        let admission = profiles
            .admit(&identity.nickname, "bind test profile")
            .await
            .unwrap();
        let (profile, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        let mut context = SessionContext::default();
        context.bind_profile(identity.clone(), profile);
        drop(lane);
        context
    }

    async fn shutdown_myroom_test(
        world: &WorldHandle,
        profile_runtime: crate::profile_io::ProfileIoRuntime,
        actor: tokio::task::JoinHandle<Result<(), crate::world::WorldSidecarError>>,
    ) {
        world.quiesce().await.unwrap();
        world.drain_sessions().await.unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
        world.shutdown().await.unwrap();
        actor.await.unwrap().unwrap();
    }

    fn rider_selection() -> RiderItemSelection {
        RiderItemSelection {
            character: 1_000,
            paint: 0,
            kart: 1,
            plate: 0,
            goggle: 0,
            balloon: 0,
            unknown1: 0,
            head_band: 0,
            head_phone: 0,
            hand_gear_left: 0,
            unknown2: 0,
            uniform: 0,
            decal: 0,
            pet: 0,
            flying_pet: 0,
            aura: 0,
            skid_mark: 0,
            special_kit: 0,
            rider_color: 0,
            bonus_card: 0,
            boss_mode_card: 0,
            kart_plant1: 0,
            kart_plant2: 0,
            kart_plant3: 0,
            kart_plant4: 0,
            unknown3: 0,
            fishing_pole: 0,
            tachometer: 0,
            dye: 0,
            kart_serial: 1,
            unknown4: 0,
            kart_coating: 0,
            kart_tail_lamp: 0,
        }
    }

    fn rider_selection_packet(selection: RiderItemSelection) -> Vec<u8> {
        let mut packet = PacketWriter::named("LoRqSetRiderItemOnPacket");
        for value in [
            selection.character,
            selection.paint,
            selection.kart,
            selection.plate,
            selection.goggle,
            selection.balloon,
            selection.unknown1,
            selection.head_band,
            selection.head_phone,
            selection.hand_gear_left,
            selection.unknown2,
            selection.uniform,
            selection.decal,
            selection.pet,
            selection.flying_pet,
            selection.aura,
            selection.skid_mark,
            selection.special_kit,
            selection.rider_color,
            selection.bonus_card,
            selection.boss_mode_card,
            selection.kart_plant1,
            selection.kart_plant2,
            selection.kart_plant3,
            selection.kart_plant4,
            selection.unknown3,
            selection.fishing_pole,
            selection.tachometer,
            selection.dye,
            selection.kart_serial,
        ] {
            packet.write_u16(value);
        }
        packet.write_u8(selection.unknown4);
        packet.write_u16(selection.kart_coating);
        packet.write_u16(selection.kart_tail_lamp);
        packet.into_inner()
    }

    struct TestLobbySession {
        source_session: SessionId,
        session: SessionId,
        identity: IdentityBinding,
        outbound: mpsc::Receiver<OutboundBatch>,
    }

    async fn register_lobby_session(
        world: &WorldHandle,
        nickname: &str,
        source_port: u16,
    ) -> TestLobbySession {
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source_session = world
            .register_session(SocketAddr::new(address, source_port))
            .await
            .unwrap();
        let claimed = world
            .claim_identity(source_session, nickname)
            .await
            .unwrap();
        let channel = ChannelBinding {
            channel_id: 67,
            game_type: 67,
        };
        let token = MigrationToken::new(source_port).unwrap();
        world
            .begin_migration(source_session, channel, token, Instant::now())
            .await
            .unwrap();
        let (session, _cancellation, outbound) = world
            .register_login_session(
                SocketAddr::new(address, source_port + 1),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .complete_migration(
                session,
                claimed.user_no,
                channel.channel_id,
                token,
                Instant::now(),
            )
            .await
            .unwrap()
            .binding;
        TestLobbySession {
            source_session,
            session,
            identity,
            outbound,
        }
    }

    fn create_room_request(name: &str) -> ChCreateRoomRequest {
        ChCreateRoomRequest {
            room_name: name.to_owned(),
            password: String::new(),
            game_type: 1,
            reserved_after_game_type: 0,
            ai_count: 0,
            room_data_header: 0,
            room_data: [0; ROOM_DATA_LENGTH],
            connection_context: [0; ROOM_CONNECTION_CONTEXT_LENGTH],
            reserved_before_ai_switch: 0,
            ai_switch: 0,
            reserved_after_ai_switch_1: 0,
            reserved_after_ai_switch_2: 0,
            reserved_tail: 0,
            reserved_last: 0,
        }
    }

    fn join_room_request(room_id: u16) -> ChJoinRoomRequest {
        ChJoinRoomRequest {
            room_id,
            password: String::new(),
            reserved: 0,
            connection_context: [0; ROOM_CONNECTION_CONTEXT_LENGTH],
        }
    }

    fn set_slot_state_packet(state: PlayerSlotState) -> Vec<u8> {
        let mut packet = PacketWriter::named("GrRequestSetSlotStatePacket");
        packet.write_i32(state as i32);
        packet.into_inner()
    }

    fn game_control_packet(state: i32, value0: u32) -> Vec<u8> {
        let mut packet = PacketWriter::named("GameControlPacket");
        packet.write_i32(state);
        packet.write_u8(0);
        packet.write_u32(value0);
        packet.into_inner()
    }

    fn ai_goal_in_packet(player_id: i32, race_time: u32) -> Vec<u8> {
        let mut packet = PacketWriter::named("GameAiGoalinPacket");
        packet.write_i32(player_id);
        packet.write_u32(race_time);
        packet.into_inner()
    }

    fn team_booster_packet(team: u8, contribution: f32) -> Vec<u8> {
        let mut packet = PacketWriter::named("GameTeamBoosterRequestAddGaugePacket");
        packet.write_u8(team);
        packet.write_f32(contribution);
        packet.into_inner()
    }

    async fn create_and_start_solo_loading(
        world: &WorldHandle,
        rider: &mut TestLobbySession,
        room_name: &str,
    ) {
        let profile = Profile::default();
        world
            .room_protocol(
                rider.session,
                RoomCommandPayload::Create {
                    request: create_room_request(room_name),
                    participant: room_participant_from_profile(&rider.identity, &profile, None)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let create_packets = rider.outbound.recv().await.unwrap().into_packets();
        assert_eq!(create_packets.len(), 1);

        let mut start = PacketWriter::named("GrRequestStartPacket");
        start.write_i32(0);
        assert_eq!(
            handle_lobby_request(
                world,
                rider.session,
                LobbyRequest::StartRoom,
                start.as_slice(),
            )
            .await
            .unwrap(),
            Vec::<Vec<u8>>::new()
        );
        let start_packets = rider.outbound.recv().await.unwrap().into_packets();
        assert_eq!(start_packets.len(), 2);
    }

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

    #[tokio::test]
    async fn partial_frame_barrier_and_outbound_quota_cannot_starve_the_read() {
        let payload = vec![0x5a; 96];
        let mut send_iv = 0xa1b7_1c9b;
        let wire = encode_encrypted(&payload, &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        let split = wire.len() / 2;
        let (mut writer, mut reader) = duplex(4);
        let (outbound_sender, mut outbound) = mpsc::channel(64);
        let (_cancellation_sender, mut cancellation) = oneshot::channel();
        let (partial_sender, partial_written) = oneshot::channel();
        let (release_sender, release) = oneshot::channel();
        let writer_task = tokio::spawn(async move {
            writer.write_all(&wire[..split]).await.unwrap();
            partial_sender.send(()).unwrap();
            for _ in 0..64 {
                outbound_sender
                    .send(OutboundBatch::single(vec![0x01]))
                    .await
                    .unwrap();
            }
            release.await.unwrap();
            writer.write_all(&wire[split..]).await.unwrap();
        });

        let mut receive_iv = 0xa1b7_1c9b;
        let frame = read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD);
        tokio::pin!(frame);
        let first =
            select_session_read_event(&mut cancellation, &mut outbound, frame.as_mut(), false)
                .await
                .unwrap();
        assert!(matches!(first, SessionReadEvent::Outbound(Some(_))));
        partial_written.await.unwrap();

        for _ in 1..MAX_OUTBOUND_BATCH_BURST {
            assert!(matches!(
                select_session_read_event(&mut cancellation, &mut outbound, frame.as_mut(), false,)
                    .await
                    .unwrap(),
                SessionReadEvent::Outbound(Some(_))
            ));
        }
        release_sender.send(()).unwrap();

        let mut priority_outbound = 0;
        let decoded = loop {
            match select_session_read_event(&mut cancellation, &mut outbound, frame.as_mut(), true)
                .await
                .unwrap()
            {
                SessionReadEvent::Frame(result) => break result.unwrap(),
                SessionReadEvent::Outbound(Some(_)) => priority_outbound += 1,
                SessionReadEvent::Outbound(None) => panic!("outbound queue closed"),
            }
            assert!(
                priority_outbound < 64,
                "a continuously ready outbound queue starved the partial frame"
            );
        };
        assert_eq!(decoded, payload);
        writer_task.await.unwrap();
    }

    #[tokio::test]
    async fn prelogin_deadline_is_absolute_and_authenticated_reads_have_an_idle_timeout() {
        let (mut writer, mut reader) = duplex(4_096);
        let mut send_iv = 0xa1b7_1c9b;
        let wire = encode_encrypted(b"auth-only", &mut send_iv, DEFAULT_MAX_PAYLOAD).unwrap();
        writer.write_all(&wire).await.unwrap();

        let deadline = time::Instant::now() + Duration::from_millis(30);
        let mut receive_iv = 0xa1b7_1c9b;
        assert_eq!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                false,
                deadline,
                Duration::from_secs(1),
            )
            .await
            .unwrap(),
            b"auth-only"
        );
        time::sleep(Duration::from_millis(40)).await;
        assert!(matches!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                false,
                deadline,
                Duration::from_secs(1),
            )
            .await,
            Err(LoginSessionError::LoginTimeout)
        ));

        assert!(matches!(
            read_session_frame(
                &mut reader,
                &mut receive_iv,
                DEFAULT_MAX_PAYLOAD,
                true,
                time::Instant::now() + Duration::from_secs(1),
                Duration::from_millis(20),
            )
            .await,
            Err(LoginSessionError::SessionIdleTimeout)
        ));
    }

    #[tokio::test]
    async fn write_timeout_bounds_a_client_that_stops_reading() {
        let (mut writer, _reader) = duplex(1);
        let result = write_session_bytes(&mut writer, &[0_u8; 64], Duration::from_millis(20)).await;
        assert!(matches!(result, Err(LoginSessionError::WriteTimeout)));
    }

    #[tokio::test]
    async fn ordered_batch_has_one_write_deadline_and_releases_shutdown_guards() {
        let operations = WireOperationGate::new();
        let request = operations.try_begin_request().unwrap();
        let outbound = operations.try_begin_outbound().unwrap();
        let batch = OutboundBatch::ordered(vec![vec![0x51; 4]; 3]).track_for_test(outbound);
        assert_eq!(operations.close_request_admission(), 1);
        assert_eq!(operations.close_outbound_admission(), 1);

        let config = ServerConfig {
            session_write_timeout: Duration::from_millis(45),
            ..ServerConfig::default()
        };
        let mut writer = DelayedPacketWriter::new(Duration::from_millis(20));
        let mut send_iv = 0x1234_5678;
        let result = async {
            let _request = request;
            write_outbound_batch(&mut writer, batch, &mut send_iv, &config).await
        }
        .await;
        assert!(matches!(result, Err(LoginSessionError::WriteTimeout)));
        assert_eq!(operations.active_counts().requests, 0);
        assert_eq!(operations.active_counts().outbound, 0);
        assert!(
            time::timeout(
                Duration::from_millis(50),
                operations.wait_for_request_drain_or_bypass(),
            )
            .await
            .unwrap()
        );
        assert!(
            time::timeout(
                Duration::from_millis(50),
                operations.wait_for_outbound_drain_or_bypass(),
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn ordered_batch_success_preserves_packet_order_and_iv_progression() {
        let expected = vec![
            vec![0x51, 0x36, 0x00, 0x01],
            vec![0x51, 0x36, 0x00, 0x02, 0x03],
            vec![0x51, 0x36, 0x00, 0x04, 0x05, 0x06],
        ];
        let batch = OutboundBatch::ordered(expected.clone());
        let config = ServerConfig::default();
        let (mut writer, mut reader) = duplex(4_096);
        let initial_iv = 0x1357_2468;
        let mut send_iv = initial_iv;
        write_outbound_batch(&mut writer, batch, &mut send_iv, &config)
            .await
            .unwrap();

        let mut receive_iv = initial_iv;
        for packet in expected {
            assert_eq!(
                read_encrypted_frame(&mut reader, &mut receive_iv, DEFAULT_MAX_PAYLOAD)
                    .await
                    .unwrap(),
                packet
            );
        }
        assert_eq!(receive_iv, send_iv);
    }

    #[tokio::test]
    async fn malformed_room_packets_are_rejected_before_world_authorization() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let context = SessionContext::default();
        let mut trailing = PacketWriter::named("ChGetRoomListRequestPacket");
        trailing.write_i32(0);
        trailing.write_u8(1);
        trailing.write_u8(0);
        trailing.write_u8(0xff);
        assert!(matches!(
            handle_room_request(
                &world,
                &profiles,
                SessionId::new(999),
                RoomProtocolRequest::RoomList,
                trailing.as_slice(),
                &context,
            )
            .await,
            Err(LoginSessionError::RoomProtocol(
                RoomProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));

        let mut invalid_page = PacketWriter::named("ChGetRoomListRequestPacket");
        invalid_page.write_i32(-1);
        invalid_page.write_u8(1);
        invalid_page.write_u8(0);
        assert!(matches!(
            handle_room_request(
                &world,
                &profiles,
                SessionId::new(999),
                RoomProtocolRequest::RoomList,
                invalid_page.as_slice(),
                &context,
            )
            .await,
            Err(LoginSessionError::RoomProtocol(
                RoomProtocolError::InvalidPage { page: -1, .. }
            ))
        ));

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_dispatch_classifies_all_lobby_requests() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_705))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "LobbyClassifier")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = SessionContext::default();

        let mut invalid_state = PacketWriter::named("GrRequestSetSlotStatePacket");
        invalid_state.write_i32(99);
        let mut invalid_team = PacketWriter::named("GrChangeTeamPacket");
        invalid_team.write_u8(99);
        let mut invalid_master = PacketWriter::named("PqRoomMasterChangePacket");
        invalid_master.write_utf16(&"x".repeat(33)).unwrap();
        let mut trailing_start = PacketWriter::named("GrRequestStartPacket");
        trailing_start.write_i32(0);
        trailing_start.write_u8(0xff);

        for packet in [
            invalid_state.into_inner(),
            invalid_team.into_inner(),
            invalid_master.into_inner(),
            trailing_start.into_inner(),
        ] {
            assert!(matches!(
                dispatch_packet(&services, &packet, &mut context).await,
                Err(LoginSessionError::LobbyProtocol(_))
            ));
        }
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn trailing_lobby_packet_cannot_mutate_actor_state() {
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let mut rider = register_lobby_session(&world, "TrailingLobby", 49_710).await;
        let profile = Profile::default();
        world
            .room_protocol(
                rider.session,
                RoomCommandPayload::Create {
                    request: create_room_request("Trailing"),
                    participant: room_participant_from_profile(&rider.identity, &profile, None)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let _create_reply = time::timeout(Duration::from_secs(1), rider.outbound.recv())
            .await
            .unwrap()
            .unwrap();

        let mut packet = set_slot_state_packet(PlayerSlotState::Ready);
        packet.push(0xff);
        assert!(matches!(
            handle_lobby_request(&world, rider.session, LobbyRequest::SetSlotState, &packet).await,
            Err(LoginSessionError::LobbyProtocol(
                LobbyProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));
        assert!(matches!(
            rider.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn expected_lobby_rejection_does_not_terminate_the_session() {
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_720))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "LobbyWithoutRoom")
            .await
            .unwrap();

        assert_eq!(
            handle_lobby_request(
                &world,
                session,
                LobbyRequest::SetSlotState,
                &set_slot_state_packet(PlayerSlotState::Ready),
            )
            .await
            .unwrap(),
            Vec::<Vec<u8>>::new()
        );
        assert_eq!(world.authorize_identity(session).await.unwrap(), identity);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rejected_start_keeps_session_alive_and_uses_actor_reply() {
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let mut owner = register_lobby_session(&world, "StartOwner", 49_740).await;
        let mut guest = register_lobby_session(&world, "StartGuest", 49_750).await;
        let profile = Profile::default();
        world
            .room_protocol(
                owner.session,
                RoomCommandPayload::Create {
                    request: create_room_request("NotReady"),
                    participant: room_participant_from_profile(&owner.identity, &profile, None)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let _ = owner.outbound.recv().await.unwrap();
        world
            .room_protocol(
                guest.session,
                RoomCommandPayload::Join {
                    request: join_room_request(1),
                    participant: room_participant_from_profile(&guest.identity, &profile, None)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let _ = guest.outbound.recv().await.unwrap();

        let mut start = PacketWriter::named("GrRequestStartPacket");
        start.write_i32(0);
        assert_eq!(
            handle_lobby_request(
                &world,
                owner.session,
                LobbyRequest::StartRoom,
                start.as_slice(),
            )
            .await
            .unwrap(),
            Vec::<Vec<u8>>::new()
        );
        let packets = owner.outbound.recv().await.unwrap().into_packets();
        assert_eq!(packets.len(), 1);
        assert_eq!(
            u32::from_le_bytes(packets[0][..4].try_into().unwrap()),
            p5136_core::adler32::packet_hash("GrReplyStartPacket")
        );
        assert_eq!(i32::from_le_bytes(packets[0][4..8].try_into().unwrap()), 2);
        assert_eq!(
            world.authorize_identity(owner.session).await.unwrap(),
            owner.identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_generation_is_rejected_before_lobby_mutation() {
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let rider = register_lobby_session(&world, "StaleLobby", 49_730).await;
        let packet = set_slot_state_packet(PlayerSlotState::Ready);

        assert!(matches!(
            handle_lobby_request(
                &world,
                rider.source_session,
                LobbyRequest::SetSlotState,
                &packet,
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == rider.source_session
        ));
        assert_eq!(
            world.authorize_identity(rider.session).await.unwrap(),
            rider.identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[test]
    fn room_physics_resolves_the_exact_catalog_base_block() {
        let catalog = test_catalog();
        let baseline =
            build_p5136_kart_physics_block(&P5136KartPhysicsSnapshot::csharp_s7_baseline())
                .unwrap();
        let mut profile = Profile::default();

        let unequipped = room_physics_metadata(&profile, None).unwrap();
        assert_eq!(unequipped.kart_id, 0);
        assert_eq!(
            unequipped.base_resolution,
            RoomKartBaseResolution::KartZeroBaseline
        );
        assert!(unequipped.fallback_reasons.is_empty());
        assert!(!unequipped.physics_fallback());
        assert_eq!(unequipped.block, baseline);

        profile.rider_item.kart = 1_450;
        let resolved = room_physics_metadata(&profile, Some(catalog.as_ref())).unwrap();
        let mut expected_snapshot = P5136KartPhysicsSnapshot::csharp_s7_baseline();
        expected_snapshot.kart = *catalog.kart_spec(1_450).unwrap();
        let expected = build_p5136_kart_physics_block(&expected_snapshot).unwrap();

        assert_eq!(resolved.kart_id, 1_450);
        assert_eq!(
            resolved.base_resolution,
            RoomKartBaseResolution::CatalogBaseSpec
        );
        assert_eq!(
            resolved.fallback_reasons,
            vec![RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected]
        );
        assert!(resolved.physics_fallback());
        assert_eq!(resolved.block.as_bytes().len(), 235);
        assert_ne!(resolved.block, baseline);
        assert_eq!(resolved.block, expected);
    }

    #[test]
    fn missing_catalog_or_kart_spec_uses_typed_baseline_fallback() {
        let catalog = test_catalog();
        let baseline =
            build_p5136_kart_physics_block(&P5136KartPhysicsSnapshot::csharp_s7_baseline())
                .unwrap();
        let mut profile = Profile::default();
        profile.rider_item.kart = 1_450;

        let missing_catalog = room_physics_metadata(&profile, None).unwrap();
        assert_eq!(
            missing_catalog.base_resolution,
            RoomKartBaseResolution::MissingCatalogFallback
        );
        assert_eq!(
            missing_catalog.fallback_reasons,
            vec![
                RoomPhysicsFallbackReason::CatalogUnavailable,
                RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected,
            ]
        );
        assert_eq!(missing_catalog.block, baseline);

        profile.rider_item.kart = 1_453;
        let missing_spec = room_physics_metadata(&profile, Some(catalog.as_ref())).unwrap();
        assert_eq!(
            missing_spec.base_resolution,
            RoomKartBaseResolution::MissingCatalogSpecFallback
        );
        assert_eq!(
            missing_spec.fallback_reasons,
            vec![
                RoomPhysicsFallbackReason::CatalogSpecUnavailable,
                RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected,
            ]
        );
        assert_eq!(missing_spec.block, baseline);
    }

    #[test]
    fn optional_physics_inputs_are_typed_without_overstating_the_base_spec() {
        let catalog = test_catalog();
        let mut profile = Profile::default();
        profile.rider_item.kart = 1_450;
        profile.rider_item.flying_pet = 83;
        profile.rider_item.kart_plant2 = 44;
        profile.rider_item.kart_plant4 = 46;
        profile.server_setting.speed_patch_use = 1;

        let resolved = room_physics_metadata(&profile, Some(catalog.as_ref())).unwrap();
        assert_eq!(
            resolved.base_resolution,
            RoomKartBaseResolution::CatalogBaseSpec
        );
        assert_eq!(
            resolved.fallback_reasons,
            vec![
                RoomPhysicsFallbackReason::FlyingPetNotApplied { item_id: 83 },
                RoomPhysicsFallbackReason::KartPlantNotApplied {
                    slot: 2,
                    item_id: 44,
                },
                RoomPhysicsFallbackReason::KartPlantNotApplied {
                    slot: 4,
                    item_id: 46,
                },
                RoomPhysicsFallbackReason::SpeedPatchNotApplied { value: 1 },
                RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected,
            ]
        );

        let mut expected_snapshot = P5136KartPhysicsSnapshot::csharp_s7_baseline();
        expected_snapshot.kart = *catalog.kart_spec(1_450).unwrap();
        assert_eq!(
            resolved.block,
            build_p5136_kart_physics_block(&expected_snapshot).unwrap()
        );
    }

    #[tokio::test]
    async fn myroom_player_slot_uses_exact_identity_and_profile_presentation() {
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let rider = register_lobby_session(&world, "MyRoomRider", 49_736).await;
        let mut profile = Profile::default();
        profile.rider.p2p_port = 48_888;
        profile.rider.rp = 51_360;
        profile.rider.club_name = "DirectClubName".to_owned();
        profile.rider.club_mark_logo = 0;
        profile.rider_item.character = 1_234;
        profile.rider_item.kart = 5_136;

        let slot = myroom_player_slot_from_profile(&rider.identity, &profile);
        assert_eq!(slot.user_no, rider.identity.user_no.get());
        assert_eq!(slot.nickname, rider.identity.nickname);
        assert_eq!(slot.p2p_address, Ipv4Addr::LOCALHOST);
        assert_eq!(slot.p2p_port, 48_888);
        assert_eq!(slot.rider_item_snapshot.len(), 65);
        assert_eq!(
            slot.rider_item_snapshot,
            rider_item_snapshot(&profile.rider_item)
        );
        assert_eq!(slot.rp, 51_360);
        assert_eq!(
            slot.club_name, "DirectClubName",
            "MyRoom writes the persisted club name directly even without a club-mark logo"
        );

        let ipv6_identity = IdentityBinding {
            source_ip: IpAddr::V6(Ipv6Addr::LOCALHOST),
            ..rider.identity.clone()
        };
        profile.rider.p2p_port = 70_000;
        let ipv6_slot = myroom_player_slot_from_profile(&ipv6_identity, &profile);
        assert_eq!(ipv6_slot.p2p_address, Ipv4Addr::UNSPECIFIED);
        assert_eq!(ipv6_slot.p2p_port, 0);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_first_dispatch_ignores_body_and_uses_actor_outbound_for_all_roles() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (outsider_session, _outsider_cancelled, mut outsider_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_735),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let outsider_identity = world
            .claim_identity(outsider_session, "SessionMyRoomFirstOutsider")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let mut outsider_context = bind_test_profile(&profiles, &outsider_identity).await;
        let store = ProfileStore::new(profile_root.path());
        let (_, owner_profile) = store
            .update(&owner.identity.nickname, |profile| {
                profile.rider.p2p_port = 41_001;
                profile.rider.rp = 111_222;
                profile.rider.club_name = "FreshOwnerClub".to_owned();
                profile.rider_item.character = 1_111;
                profile.rider_item.kart = 2_222;
            })
            .unwrap();
        let (_, visitor_profile) = store
            .update(&visitor.identity.nickname, |profile| {
                profile.rider.p2p_port = 41_002;
                profile.rider.rp = 333_444;
                profile.rider.club_name = "FreshVisitorClub".to_owned();
                profile.rider_item.character = 3_333;
                profile.rider_item.kart = 4_444;
            })
            .unwrap();
        let mut expected_slots: [MyRoomSlot; MYROOM_SLOT_COUNT] =
            std::array::from_fn(|_| MyRoomSlot::Empty);
        expected_slots[0] = MyRoomSlot::Player(myroom_player_slot_from_profile(
            &owner.identity,
            &owner_profile,
        ));
        expected_slots[1] = MyRoomSlot::Player(myroom_player_slot_from_profile(
            &visitor.identity,
            &visitor_profile,
        ));
        let expected_member_packet = serialize_slot_data(&expected_slots).unwrap();
        let mut request = PacketWriter::named("RmFirstRequestPacket").into_inner();
        request.extend_from_slice(&[0x00, 0xff, 0x51, 0x36, 0xaa]);

        for (session_id, context) in [
            (owner.session, &mut owner_context),
            (visitor.session, &mut visitor_context),
            (outsider_session, &mut outsider_context),
        ] {
            let services = SessionServices {
                config: &config,
                world: &world,
                profiles: &profiles,
                session_id,
            };
            assert!(
                dispatch_packet(&services, &request, context)
                    .await
                    .unwrap()
                    .is_empty(),
                "FirstState is delivered only through the actor-owned outbound queue"
            );
        }

        let owner_packets = owner.outbound.try_recv().unwrap().into_packets();
        let visitor_packets = visitor.outbound.try_recv().unwrap().into_packets();
        let outsider_packets = outsider_outbound.try_recv().unwrap().into_packets();
        assert_eq!(owner_packets.len(), 1);
        assert_eq!(visitor_packets, owner_packets);
        assert_eq!(
            owner_packets[0], expected_member_packet,
            "FirstState must reload every occupied slot instead of using the entry-time Hub cache"
        );
        assert_eq!(outsider_packets.len(), 1);
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = std::array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(outsider_packets[0], serialize_slot_data(&empty).unwrap());
        assert_ne!(owner_packets, outsider_packets);
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        assert!(outsider_outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_secede_dispatch_is_actor_owned_and_replies_for_member_and_nonmember() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let mut secede = PacketWriter::named("ChRqSecedeMyRoomPacket").into_inner();
        secede.extend_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
        let visitor_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };

        assert!(
            dispatch_packet(&visitor_services, &secede, &mut visitor_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_secede_reply()]
        );
        let post_leave = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(post_leave.len(), 1);

        let first = PacketWriter::named("RmFirstRequestPacket").into_inner();
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        assert!(
            dispatch_packet(&owner_services, &first, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            post_leave,
            "Secede peer fan-out must be the same post-leave snapshot returned by FirstState"
        );

        assert!(
            dispatch_packet(&visitor_services, &secede, &mut visitor_context)
                .await
                .unwrap()
                .is_empty(),
            "a nonmember Secede remains a protocol success"
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_secede_reply()]
        );
        assert!(
            owner.outbound.try_recv().is_err(),
            "nonmember Secede must not publish a peer snapshot"
        );

        assert!(
            dispatch_packet(&owner_services, &secede, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_secede_reply()]
        );
        assert!(visitor.outbound.try_recv().is_err());

        assert!(
            dispatch_packet(&owner_services, &first, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = std::array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_slot_data(&empty).unwrap()]
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_owner_secede_broadcast_reloads_live_tombstone_profile() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let _visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let store = ProfileStore::new(profile_root.path());
        let (_, owner_profile) = store
            .update(&owner.identity.nickname, |profile| {
                profile.rider.p2p_port = 42_001;
                profile.rider.rp = 515_136;
                profile.rider.club_name = "FreshTombstone".to_owned();
                profile.rider_item.character = 5_001;
                profile.rider_item.kart = 5_002;
            })
            .unwrap();
        let visitor_profile = store
            .load_or_create(&visitor.identity.nickname)
            .unwrap()
            .profile;
        let mut expected_slots: [MyRoomSlot; MYROOM_SLOT_COUNT] =
            std::array::from_fn(|_| MyRoomSlot::Empty);
        expected_slots[0] = MyRoomSlot::Player(myroom_player_slot_from_profile(
            &owner.identity,
            &owner_profile,
        ));
        expected_slots[1] = MyRoomSlot::Player(myroom_player_slot_from_profile(
            &visitor.identity,
            &visitor_profile,
        ));

        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let mut secede = PacketWriter::named("ChRqSecedeMyRoomPacket").into_inner();
        secede.extend_from_slice(&[0xa5, 0x51, 0x36]);
        assert!(
            dispatch_packet(&services, &secede, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_secede_reply()]
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_slot_data(&expected_slots).unwrap()],
            "owner leave must render the retained slot-zero tombstone from the latest disk profile"
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_info_dispatch_redacts_visitor_secrets_and_skips_visitor_body() {
        let initial = MyRoomInfo {
            room_id: 17,
            bgm: 3,
            use_room_password: 1,
            room_password: "initial room".to_owned(),
            use_item_password: 1,
            item_password: "initial item".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(initial.clone());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            owner: _owner,
            visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let visitor_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        let malformed_visitor_packet = PacketWriter::named("RmNotiMyRoomInfoPacket").into_inner();
        let mut redacted = initial.clone();
        redacted.room_password.clear();
        redacted.item_password.clear();
        assert_eq!(
            dispatch_packet(
                &visitor_services,
                &malformed_visitor_packet,
                &mut visitor_context,
            )
            .await
            .unwrap(),
            vec![serialize_myroom_info(&redacted).unwrap()],
            "a visitor receives policy flags but never the owner's raw secrets"
        );

        let outsider_session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_737))
            .await
            .unwrap();
        let outsider_identity = world
            .claim_identity(outsider_session, "SessionMyRoomOutsider")
            .await
            .unwrap();
        let mut outsider_context = bind_test_profile(&profiles, &outsider_identity).await;
        let outsider_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: outsider_session,
        };
        assert!(
            dispatch_packet(
                &outsider_services,
                &malformed_visitor_packet,
                &mut outsider_context,
            )
            .await
            .unwrap()
            .is_empty(),
            "a nonmember is ignored before its malformed body is parsed"
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::too_many_lines,
        reason = "the exact ordered response test keeps disk fixtures, wire expectations, and both requester roles together"
    )]
    async fn myroom_request_items_publishes_one_ordered_owner_snapshot_to_each_requester() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut owner_profile = Profile::default();
        owner_profile.server_setting.prevent_item_use = 1;
        let owner_saved = store
            .save(&owner.identity.nickname, &owner_profile)
            .unwrap();
        let visitor_saved = store
            .save(&visitor.identity.nickname, &Profile::default())
            .unwrap();
        let owner_directory = owner_saved.path.parent().unwrap();
        fs::write(
            owner_directory.join("TuneData.json"),
            br#"[{"ID":101,"SN":2,"Tune1":3,"Tune2":4,"Tune3":5,"Slot1":6,"Count1":7,"Slot2":8,"Count2":9}]"#,
        )
        .unwrap();
        fs::write(
            owner_directory.join("NewKart.json"),
            br#"[{"KartID":5136,"KartSN":17}]"#,
        )
        .unwrap();
        fs::write(
            owner_directory.join("PartsData.json"),
            br#"[{"ID":201,"SN":19,"Engine":11,"EngineGrade":12,"EngineValue":13}]"#,
        )
        .unwrap();
        fs::write(
            visitor_saved.path.parent().unwrap().join("NewKart.json"),
            br#"[{"KartID":999,"KartSN":1}]"#,
        )
        .unwrap();

        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        let tunes = [MyRoomTune {
            item_id: 101,
            serial_number: 2,
            tune_1: 3,
            tune_2: 4,
            tune_3: 5,
            slot_1: 6,
            count_1: 7,
            slot_2: 8,
            count_2: 9,
        }];
        let karts = [MyRoomKart {
            kart_id: 5136,
            serial_number: 17,
        }];
        let parts = [MyRoomParts {
            item_id: 201,
            serial_number: 19,
            engine: 11,
            engine_grade: 12,
            engine_value: 13,
            ..MyRoomParts::default()
        }];
        let mut expected = serialize_owner_item_enchants(&tunes).unwrap();
        expected.extend(serialize_owner_items(&karts, &parts, true).unwrap());
        assert_eq!(expected.len(), 3);

        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        assert!(
            dispatch_packet(&owner_services, &request, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(owner.outbound.try_recv().unwrap().into_packets(), expected);
        assert!(
            visitor.outbound.try_recv().is_err(),
            "owner-item responses are requester-only, never room broadcasts"
        );

        let visitor_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        assert!(
            dispatch_packet(&visitor_services, &request, &mut visitor_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            expected,
            "a public visitor must read the room owner's snapshot, not its own sidecars"
        );
        assert!(owner.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_request_items_strictly_rejects_trailing_data_and_types_missing_owner() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            owner: _owner,
            visitor: _visitor,
        } = fixture;
        let (outsider_session, _cancelled, mut outsider_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_739),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let outsider_identity = world
            .claim_identity(outsider_session, "SessionMyRoomItemOutsider")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut context = bind_test_profile(&profiles, &outsider_identity).await;
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: outsider_session,
        };
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        let mut malformed = request.clone();
        malformed.push(0xa5);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes {
                    name: REQUEST_MYROOM_ITEMS_NAME,
                    count: 1,
                }
            ))
        ));
        assert!(
            outsider_outbound.try_recv().is_err(),
            "a malformed request cannot publish any partial response"
        );

        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            outsider_outbound.try_recv().unwrap().into_packets(),
            vec![serialize_missing_owner_items()]
        );
        assert!(outsider_outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_request_items_distinguishes_empty_inventory_from_missing_owner() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let _owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        assert!(
            dispatch_packet(&services, &request, &mut visitor_context)
                .await
                .unwrap()
                .is_empty()
        );
        let expected_empty = serialize_owner_items(&[], &[], false).unwrap();
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            expected_empty
        );
        assert_ne!(
            expected_empty,
            vec![serialize_missing_owner_items()],
            "an existing owner with an empty inventory is not a missing owner"
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn protected_myroom_items_are_denied_before_disk_read_and_secrets_are_redacted() {
        let protected = MyRoomInfo {
            use_room_password: 1,
            room_password: "room secret".to_owned(),
            use_item_password: 1,
            item_password: "item secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(protected);
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let owner_saved = store
            .save(&owner.identity.nickname, &Profile::default())
            .unwrap();
        store
            .save(&visitor.identity.nickname, &Profile::default())
            .unwrap();
        fs::write(
            owner_saved.path.parent().unwrap().join("NewKart.json"),
            b"[{",
        )
        .unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        assert!(
            dispatch_packet(&services, &request, &mut visitor_context)
                .await
                .unwrap()
                .is_empty(),
            "the protected visitor path must not touch the malformed owner sidecar"
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_missing_owner_items()]
        );

        let view = world
            .myroom_session_view(visitor.session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.info().use_room_password, 1);
        assert_eq!(view.info().use_item_password, 1);
        assert!(view.info().room_password.is_empty());
        assert!(view.info().item_password.is_empty());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_request_items_backpressure_is_atomic_and_retryable() {
        let fixture = spawn_myroom_world_with_outbound_capacity(MyRoomInfo::default(), 1);
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            visitor: _visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        assert!(
            dispatch_packet(&services, &request, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            dispatch_packet(&services, &request, &mut owner_context).await,
            Err(LoginSessionError::World(
                WorldError::MyRoomCommandOutboundUnavailable { session }
            )) if session == owner.session
        ));

        let expected = serialize_owner_items(&[], &[], false).unwrap();
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            expected,
            "the failed publication cannot alter the already queued batch"
        );
        assert!(owner.outbound.try_recv().is_err());

        assert!(
            dispatch_packet(&services, &request, &mut owner_context)
                .await
                .unwrap()
                .is_empty(),
            "draining the single queue slot makes the same request retryable"
        );
        assert_eq!(owner.outbound.try_recv().unwrap().into_packets(), expected);
        assert!(owner.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_myroom_item_request_retains_identity_until_profile_read_finishes() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            visitor: _visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let hook = BlockingUpdateHook::new();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let profiles = profiles.with_blocking_owner_item_hook(Arc::clone(&hook));
        let owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let destination = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 50_101))
            .await
            .unwrap();
        let token = MigrationToken::new(0x5137).unwrap();
        world
            .begin_migration(
                owner.session,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        let request_world = world.clone();
        let request_profiles = profiles.clone();
        let request_session = owner.session;
        let mut request_context = owner_context;
        let request = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        let request_task = tokio::spawn(async move {
            let config = ServerConfig::default();
            let services = SessionServices {
                config: &config,
                world: &request_world,
                profiles: &request_profiles,
                session_id: request_session,
            };
            dispatch_packet(&services, &request, &mut request_context).await
        });
        let entered_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || entered_hook.entered.wait())
            .await
            .unwrap();

        request_task.abort();
        assert!(request_task.await.unwrap_err().is_cancelled());

        let migration_world = world.clone();
        let migration_profiles = profiles.clone();
        let user_no = owner.identity.user_no;
        let nickname = owner.identity.nickname.clone();
        let (attempting, attempted) = oneshot::channel();
        let mut migration = tokio::spawn(async move {
            let preflight = migration_world
                .preflight_migration(destination, user_no, 12, token, Instant::now())
                .await
                .unwrap();
            let _ = attempting.send(());
            preflight.wait_for_operations_drained().await.unwrap();
            let admission = migration_profiles
                .admit(&nickname, "test RequestItems migration handoff")
                .await
                .unwrap();
            let (profile, lane) = migration_profiles
                .load(nickname, false, admission)
                .await
                .unwrap();
            let profile =
                MyRoomProfileLease::new(myroom_profile_presentation(&profile.profile), lane);
            migration_world
                .complete_preflighted_migration(preflight, profile)
                .await
        });
        attempted.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(50), &mut migration)
                .await
                .is_err(),
            "migration drained while the cancelled RequestItems disk read still owned its child lease"
        );

        let release_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || release_hook.release.wait())
            .await
            .unwrap();
        migration.await.unwrap().unwrap();
        assert!(
            owner.outbound.try_recv().is_err(),
            "a cancelled request cannot publish its completed disk result"
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_owner_info_dispatch_persists_before_exact_echo_and_context_refresh() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let proposed = MyRoomInfo {
            room_id: 5136,
            bgm: 7,
            use_room_password: 1,
            room_password: "durable room".to_owned(),
            item_password: "durable item".to_owned(),
            kart_1: 1450,
            kart_2: 1453,
            ..MyRoomInfo::default()
        };
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        assert!(
            dispatch_packet(
                &owner_services,
                &serialize_myroom_info(&proposed).unwrap(),
                &mut owner_context,
            )
            .await
            .unwrap()
            .is_empty(),
            "the actor-owned outbound queue carries the owner's echo"
        );
        let echo = time::timeout(Duration::from_secs(1), owner.outbound.recv())
            .await
            .unwrap()
            .unwrap()
            .into_packets();
        assert_eq!(echo, vec![serialize_myroom_info(&proposed).unwrap()]);
        assert!(owner.outbound.try_recv().is_err());
        assert!(
            visitor.outbound.try_recv().is_err(),
            "owner info updates echo only to the owner"
        );

        let current = world.authorize_identity(owner.session).await.unwrap();
        assert_eq!(
            owner_context
                .profile_for(&current)
                .unwrap()
                .my_room
                .try_to_protocol_info()
                .unwrap(),
            proposed
        );
        assert_eq!(
            owner_context
                .bound_profile_for(&current)
                .unwrap()
                .profile
                .revision,
            Some(2)
        );
        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create(&owner.identity.nickname)
            .unwrap();
        assert_eq!(persisted.revision, Some(2));
        assert_eq!(
            persisted.profile.my_room.try_to_protocol_info().unwrap(),
            proposed
        );
        assert_eq!(
            world
                .myroom_session_view(owner.session)
                .await
                .unwrap()
                .unwrap()
                .info(),
            &proposed
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[test]
    fn myroom_owner_item_operational_limits_are_checked_from_the_wire_plan() {
        let maximum_loader_response = plan_owner_item_packets(
            MAX_MYROOM_ITEM_RECORDS,
            MAX_MYROOM_ITEM_RECORDS,
            MAX_MYROOM_ITEM_RECORDS,
        )
        .unwrap();
        assert_eq!(maximum_loader_response.packet_count(), 7_563);
        assert_eq!(maximum_loader_response.byte_len(), 5_555_382);
        assert_eq!(MAX_MYROOM_OWNER_ITEM_PACKETS, 7_563);
        assert!(
            MyRoomOwnerItemPacketBatch::enforce_wire_plan(
                maximum_loader_response.packet_count(),
                maximum_loader_response.byte_len(),
                MAX_MYROOM_OWNER_ITEM_PACKETS,
                MAX_MYROOM_OWNER_ITEM_BYTES,
            )
            .is_ok()
        );

        assert!(matches!(
            MyRoomOwnerItemPacketBatch::enforce_wire_plan(
                MAX_MYROOM_OWNER_ITEM_PACKETS + 1,
                maximum_loader_response.byte_len(),
                MAX_MYROOM_OWNER_ITEM_PACKETS,
                MAX_MYROOM_OWNER_ITEM_BYTES,
            ),
            Err(LoginSessionError::MyRoomOwnerItemPacketLimit {
                actual,
                maximum: MAX_MYROOM_OWNER_ITEM_PACKETS,
            }) if actual == MAX_MYROOM_OWNER_ITEM_PACKETS + 1
        ));

        let small = plan_owner_item_packets(0, 1, 0).unwrap();
        assert!(matches!(
            MyRoomOwnerItemPacketBatch::enforce_wire_plan(
                small.packet_count(),
                small.byte_len(),
                usize::MAX,
                small.byte_len() - 1,
            ),
            Err(LoginSessionError::MyRoomOwnerItemByteLimit {
                actual,
                maximum,
            }) if actual == small.byte_len() && maximum == small.byte_len() - 1
        ));
    }

    #[tokio::test]
    async fn myroom_owner_profile_load_is_canonical_and_conversion_errors_are_typed() {
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut profile = Profile::default();
        profile.my_room.my_room = 17;
        profile.my_room.my_room_bgm = 3;
        profile.my_room.room_pwd = "room".to_owned();
        profile.my_room.item_pwd = "item".to_owned();
        store.save("MyRoomOwner", &profile).unwrap();

        let (profiles, runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let admission = profiles
            .admit("myroomowner", "test MyRoom owner profile load")
            .await
            .unwrap();
        let (snapshot, info, lane) = profiles
            .load_myroom_owner_profile("MYROOMOWNER".to_owned(), admission)
            .await
            .unwrap();
        assert_eq!(snapshot.revision, Some(1));
        assert_eq!(snapshot.profile.my_room.my_room, 17);
        assert_eq!(info.room_id, 17);
        assert_eq!(info.bgm, 3);
        assert_eq!(info.room_password, "room");
        assert_eq!(info.item_password, "item");
        drop(lane);
        runtime.shutdown().await.unwrap();

        let invalid_root = tempfile::tempdir().unwrap();
        let invalid_store = ProfileStore::new(invalid_root.path());
        let mut invalid = Profile::default();
        invalid.my_room.room_pwd = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        invalid_store.save("InvalidMyRoom", &invalid).unwrap();
        let (profiles, runtime) =
            ProfileCoordinator::new_test(invalid_root.path().to_owned(), None);
        let admission = profiles
            .admit("InvalidMyRoom", "test invalid MyRoom conversion")
            .await
            .unwrap();
        assert!(matches!(
            profiles
                .load_myroom_owner_profile("invalidmyroom".to_owned(), admission)
                .await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::StringTooLong {
                    field: "MyRoom room password",
                    ..
                }
            ))
        ));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn myroom_owner_item_read_keeps_parts_without_karts_and_types_sidecar_errors() {
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut profile = Profile::default();
        profile.server_setting.prevent_item_use = 1;
        let saved = store.save("ItemOwner", &profile).unwrap();
        let rider_directory = saved.path.parent().unwrap();
        let parts_path = rider_directory.join("PartsData.json");
        let karts_path = rider_directory.join("NewKart.json");
        fs::write(
            &parts_path,
            br#"[{"ID":5136,"SN":1,"Engine":2,"EngineGrade":3,"EngineValue":4}]"#,
        )
        .unwrap();

        let (profiles, runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let admission = profiles
            .admit("itemowner", "test MyRoom owner item load")
            .await
            .unwrap();
        let (batch, lane) = profiles
            .load_myroom_owner_items("ITEMOWNER".to_owned(), admission)
            .await
            .unwrap();
        let packets = batch.into_packets();
        assert_eq!(packets.len(), 1);
        assert_eq!(packets[0].len(), 72);
        assert_eq!(
            u32::from_le_bytes(packets[0][..4].try_into().unwrap()),
            p5136_core::adler32::packet_hash(OWNER_ITEM_NAME)
        );
        assert_eq!(
            i16::from_le_bytes(packets[0][24..26].try_into().unwrap()),
            5136
        );
        drop(lane);

        fs::write(
            &karts_path,
            br#"[{"KartID":5136,"KartSN":7,"FutureField":true}]"#,
        )
        .unwrap();
        let admission = profiles
            .admit("ItemOwner", "test deterministic MyRoom prevent-item flag")
            .await
            .unwrap();
        let (batch, lane) = profiles
            .load_myroom_owner_items("itemowner".to_owned(), admission)
            .await
            .unwrap();
        let packets = batch.into_packets();
        assert_eq!(packets.len(), 2);
        assert_eq!(
            u32::from_le_bytes(packets[0][..4].try_into().unwrap()),
            p5136_core::adler32::packet_hash(OWNER_ITEM_NAME)
        );
        assert_eq!(packets[0][24], 1);
        drop(lane);

        fs::write(&parts_path, b"[{").unwrap();
        let admission = profiles
            .admit("ItemOwner", "test malformed MyRoom owner item load")
            .await
            .unwrap();
        assert!(matches!(
            profiles
                .load_myroom_owner_items("itemowner".to_owned(), admission)
                .await,
            Err(LoginSessionError::MyRoomItemState(
                MyRoomItemStateError::Json { path, .. }
            )) if path == parts_path
        ));
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn equipment_publish_does_not_refresh_admission_physics_snapshot() {
        let catalog = test_catalog();
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let mut rider = register_lobby_session(&world, "PhysicsSnapshot", 49_735).await;
        let mut profile = Profile::default();
        profile.rider_item.kart = 1_450;
        let participant =
            room_participant_from_profile(&rider.identity, &profile, Some(catalog.as_ref()))
                .unwrap();
        let admission_physics = participant.kart_physics.clone();
        world
            .room_protocol(
                rider.session,
                RoomCommandPayload::Create {
                    request: create_room_request("PhysicsSnapshot"),
                    participant,
                },
            )
            .await
            .unwrap();
        let _create_reply = rider.outbound.recv().await.unwrap();

        profile.rider_item.kart = 0;
        world
            .publish_room_equipment(rider.session, rider_item_snapshot(&profile.rider_item))
            .await
            .unwrap();

        let mut start = PacketWriter::named("GrRequestStartPacket");
        start.write_i32(0);
        handle_lobby_request(
            &world,
            rider.session,
            LobbyRequest::StartRoom,
            start.as_slice(),
        )
        .await
        .unwrap();
        let start_packets = rider.outbound.recv().await.unwrap().into_packets();
        let command = &start_packets[1];
        let baseline =
            build_p5136_kart_physics_block(&P5136KartPhysicsSnapshot::csharp_s7_baseline())
                .unwrap();
        assert!(
            command
                .windows(admission_physics.as_bytes().len())
                .any(|window| window == admission_physics.as_bytes())
        );
        assert!(
            !command
                .windows(baseline.as_bytes().len())
                .any(|window| window == baseline.as_bytes())
        );

        world.session_closed(rider.session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_dispatch_classifies_all_race_requests() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_760))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "RaceClassifier")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = SessionContext::default();

        let mut truncated_control = PacketWriter::named("GameControlPacket");
        truncated_control.write_i32(0);
        truncated_control.write_u8(0);
        let invalid_ai = ai_goal_in_packet(16, 100);
        let invalid_team = team_booster_packet(3, 1.0);

        for packet in [truncated_control.into_inner(), invalid_ai, invalid_team] {
            assert!(matches!(
                dispatch_packet(&services, &packet, &mut context).await,
                Err(LoginSessionError::RaceProtocol(_))
            ));
        }
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn malformed_race_packets_cannot_mutate_actor_state() {
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let mut rider = register_lobby_session(&world, "MalformedRace", 49_770).await;
        create_and_start_solo_loading(&world, &mut rider, "MalformedRace").await;

        let mut oversized_control = game_control_packet(0, 100);
        oversized_control.extend_from_slice(&[0; 257]);
        assert!(matches!(
            handle_race_request(
                &world,
                rider.session,
                RaceRequest::GameControl,
                &oversized_control,
            )
            .await,
            Err(LoginSessionError::RaceProtocol(
                RaceProtocolError::GameControlTailTooLarge {
                    actual: 257,
                    maximum: 256,
                }
            ))
        ));

        let mut trailing_ai = ai_goal_in_packet(0, 100);
        trailing_ai.push(0xff);
        assert!(matches!(
            handle_race_request(&world, rider.session, RaceRequest::AiGoalIn, &trailing_ai,).await,
            Err(LoginSessionError::RaceProtocol(
                RaceProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));

        let mut trailing_booster = team_booster_packet(1, 1.0);
        trailing_booster.push(0xff);
        assert!(matches!(
            handle_race_request(
                &world,
                rider.session,
                RaceRequest::TeamBoosterGauge,
                &trailing_booster,
            )
            .await,
            Err(LoginSessionError::RaceProtocol(
                RaceProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));
        assert!(matches!(
            rider.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));

        assert!(matches!(
            world
                .race_command(
                    rider.session,
                    RaceCommandPayload::GameControl(GameControlRequest {
                        state: 0,
                        optional_pair: None,
                        value0: 100,
                        trailing: Vec::new(),
                    }),
                )
                .await
                .unwrap(),
            RaceCommandOutcome::LoadingAwaiting {
                expected_participants: 1,
                ..
            }
        ));

        world.session_closed(rider.session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn game_control_state_zero_reaches_actor_without_direct_response() {
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let mut rider = register_lobby_session(&world, "RaceStateZero", 49_780).await;
        create_and_start_solo_loading(&world, &mut rider, "RaceStateZero").await;
        let packet = game_control_packet(0, 0x1234_5678);

        assert_eq!(
            handle_race_request(&world, rider.session, RaceRequest::GameControl, &packet,)
                .await
                .unwrap(),
            Vec::<Vec<u8>>::new()
        );
        assert!(matches!(
            rider.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            world
                .race_command(
                    rider.session,
                    RaceCommandPayload::GameControl(parse_game_control_request(&packet).unwrap()),
                )
                .await
                .unwrap(),
            RaceCommandOutcome::IgnoredDuplicate { .. }
        ));

        world.session_closed(rider.session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_generation_is_rejected_before_race_mutation() {
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let rider = register_lobby_session(&world, "StaleRace", 49_790).await;
        let packet = game_control_packet(0, 100);

        assert!(matches!(
            handle_race_request(
                &world,
                rider.source_session,
                RaceRequest::GameControl,
                &packet,
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == rider.source_session
        ));
        assert_eq!(
            world.authorize_identity(rider.session).await.unwrap(),
            rider.identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn expected_race_rejections_do_not_terminate_the_session() {
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let mut rider = register_lobby_session(&world, "RaceRejection", 49_800).await;
        let profile = Profile::default();
        world
            .room_protocol(
                rider.session,
                RoomCommandPayload::Create {
                    request: create_room_request("RaceRejection"),
                    participant: room_participant_from_profile(&rider.identity, &profile, None)
                        .unwrap(),
                },
            )
            .await
            .unwrap();
        let _ = rider.outbound.recv().await.unwrap();

        for (request, packet) in [
            (RaceRequest::GameControl, game_control_packet(0, 100)),
            (RaceRequest::AiGoalIn, ai_goal_in_packet(0, 100)),
            (RaceRequest::TeamBoosterGauge, team_booster_packet(1, 1.0)),
        ] {
            assert_eq!(
                handle_race_request(&world, rider.session, request, &packet)
                    .await
                    .unwrap(),
                Vec::<Vec<u8>>::new()
            );
        }
        assert!(matches!(
            rider.outbound.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert_eq!(
            world.authorize_identity(rider.session).await.unwrap(),
            rider.identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn malformed_plant_request_requires_current_identity_and_returns_exact_failure() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let source = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_700))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_701))
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "MalformedOwner")
            .await
            .unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (profile, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        let mut context = SessionContext::default();
        context.bind_profile(identity.clone(), profile);
        drop(lane);
        let mut truncated = PacketWriter::named("PqEquipTuningExPacket");
        truncated.write_i16(43);

        let responses = handle_equipment_request(
            &world,
            &profiles,
            source,
            EquipmentRequest::EquipPlantPart,
            truncated.as_slice(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(responses, vec![serialize_equip_tuning_failure()]);
        assert!(matches!(
            handle_equipment_request(
                &world,
                &profiles,
                SessionId::new(999),
                EquipmentRequest::EquipPlantPart,
                truncated.as_slice(),
                &mut context,
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::UnauthenticatedSession(id)
            ))) if id == SessionId::new(999)
        ));

        let token = MigrationToken::new(0x5138).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        world
            .complete_migration(destination, identity.user_no, 12, token, Instant::now())
            .await
            .unwrap();
        assert!(matches!(
            handle_equipment_request(
                &world,
                &profiles,
                source,
                EquipmentRequest::EquipPlantPart,
                truncated.as_slice(),
                &mut context,
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn owned_plant_request_persists_sidecar_before_exact_success() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_800))
            .await
            .unwrap();
        let identity = world.claim_identity(session, "PlantOwner").await.unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (profile, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        let rider_directory = profile.source_path.parent().unwrap().to_owned();
        let mut context = SessionContext::default();
        context.bind_profile(identity, profile);
        drop(lane);
        let request = PlantPartEquipRequest {
            item_category: 43,
            item_id: 1_000,
            kart_category: 3,
            kart_id: 1,
            kart_serial: 1,
        };
        let mut packet = PacketWriter::named("PqEquipTuningExPacket");
        packet.write_i16(request.item_category);
        packet.write_i16(request.item_id);
        packet.write_i16(request.kart_category);
        packet.write_i16(request.kart_id);
        packet.write_i16(request.kart_serial);

        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::EquipPlantPart,
            packet.as_slice(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(responses, vec![serialize_equip_tuning_success(request)]);
        let equipment = EquipmentExceptions::load(profile_root.path(), rider_directory).unwrap();
        assert_eq!(equipment.plant.len(), 1);
        assert_eq!(equipment.plant[0].id, 1);
        assert_eq!(equipment.plant[0].engine_category, 43);
        assert_eq!(equipment.plant[0].engine_id, 1_000);

        world.session_closed(session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rider_request_silently_ignores_short_body_and_ignores_trailing_bytes() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_900))
            .await
            .unwrap();
        let identity = world.claim_identity(session, "RiderOwner").await.unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (profile, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        let mut context = SessionContext::default();
        context.bind_profile(identity, profile);
        drop(lane);
        ProfileStore::new(profile_root.path())
            .update("RiderOwner", |profile| {
                profile.rider.premium = 42;
                profile.rider.rp = 51_360;
                profile.rider.club_name = "FreshEquipmentContext".to_owned();
                profile.game_option.screen = 7;
            })
            .unwrap();
        let selection = rider_selection();
        let packet = rider_selection_packet(selection);
        let truncated = &packet[..packet.len() - 1];

        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::SetRiderItems,
            truncated,
            &mut context,
        )
        .await
        .unwrap();
        assert!(responses.is_empty());
        let unchanged = ProfileStore::new(profile_root.path())
            .load_or_create("RiderOwner")
            .unwrap();
        assert_eq!(unchanged.revision, Some(2));
        assert_eq!(unchanged.profile.rider_item, Profile::default().rider_item);

        let mut trailing = packet.clone();
        trailing.extend_from_slice(&[0x51, 0x36]);

        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::SetRiderItems,
            &trailing,
            &mut context,
        )
        .await
        .unwrap();
        assert!(responses.is_empty());
        let current_identity = world.authorize_identity(session).await.unwrap();
        let current_profile = context.profile_for(&current_identity).unwrap();
        assert_eq!(current_profile.rider_item.character, selection.character);
        assert_eq!(current_profile.rider_item.kart, selection.kart);
        assert_eq!(current_profile.rider.premium, 42);
        assert_eq!(current_profile.rider.rp, 51_360);
        assert_eq!(current_profile.rider.club_name, "FreshEquipmentContext");
        assert_eq!(current_profile.game_option.screen, 7);
        assert_eq!(
            context
                .bound_profile_for(&current_identity)
                .unwrap()
                .profile
                .revision,
            Some(3)
        );
        let expected_snapshot: [u8; 65] = packet[4..].try_into().unwrap();
        assert_eq!(
            rider_item_snapshot(&current_profile.rider_item),
            expected_snapshot
        );
        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("RiderOwner")
            .unwrap();
        assert_eq!(persisted.revision, Some(3));
        assert_eq!(persisted.profile.rider_item.character, selection.character);

        world.session_closed(session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[allow(
        clippy::too_many_lines,
        reason = "the end-to-end test covers both valid and wire-invalid GetRider refreshes against one tracked MyRoom"
    )]
    async fn get_rider_reloads_disk_and_silently_refreshes_the_myroom_cache() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let myroom = spawn_myroom_world(MyRoomInfo::default());
        let world = myroom.handle;
        let actor = myroom.actor;
        let mut owner = myroom.owner;
        let mut visitor = myroom.visitor;
        let session = owner.session;
        let identity = owner.identity.clone();
        let mut context = bind_test_profile(&profiles, &identity).await;
        let stale_premium = context.profile_for(&identity).unwrap().rider.premium;
        assert_ne!(stale_premium, 42);
        ProfileStore::new(profile_root.path())
            .update(&identity.nickname, |profile| {
                profile.rider.premium = 42;
                profile.rider.p2p_port = 45_137;
                profile.rider.rp = 515_137;
                profile.rider.club_name = "FreshGetRider".to_owned();
                profile.rider_item.character = 1_000;
                profile.rider_item.kart = 1;
                profile.rider_item.kart_serial = 0;
                profile.rider_item.unknown4 = 0xD5;
                profile.rider_item.kart_coating = 0xD6D7;
                profile.rider_item.kart_tail_lamp = 0xD8D9;
            })
            .unwrap();
        assert_eq!(
            context.profile_for(&identity).unwrap().rider.premium,
            stale_premium,
            "the test must begin with a stale in-memory snapshot"
        );

        let responses = handle_get_rider(&world, &profiles, session, &mut context)
            .await
            .unwrap();
        assert!(!responses.is_empty());
        let current = world.authorize_identity(session).await.unwrap();
        let current_profile = context.profile_for(&current).unwrap();
        assert_eq!(current_profile.rider.premium, 42);
        assert_eq!(current_profile.rider.p2p_port, 45_137);
        assert_eq!(current_profile.rider.rp, 515_137);
        assert_eq!(current_profile.rider.club_name, "FreshGetRider");
        assert_eq!(current_profile.rider_item.kart, 1);
        assert_eq!(current_profile.rider_item.kart_serial, 1);
        assert_eq!(current_profile.rider_item.unknown4, 0xD5);
        assert_eq!(current_profile.rider_item.kart_tail_lamp, 0xD8D9);
        let mut rider_reader = PacketReader::new(responses.last().unwrap());
        assert_eq!(
            rider_reader.read_u32().unwrap(),
            adler32::packet_hash("PrGetRider")
        );
        assert_eq!(rider_reader.read_u8().unwrap(), 1);
        assert_eq!(rider_reader.read_u8().unwrap(), 0);
        assert_eq!(rider_reader.read_utf16().unwrap(), identity.nickname);
        for _ in 0..5 {
            rider_reader.read_u16().unwrap();
        }
        assert_eq!(
            rider_reader
                .read_bytes(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH)
                .unwrap(),
            rider_item_snapshot(&current_profile.rider_item)
        );
        assert!(
            owner.outbound.try_recv().is_err(),
            "GetRider must not immediately publish a MyRoom snapshot to its requester"
        );
        assert!(
            visitor.outbound.try_recv().is_err(),
            "GetRider must not immediately publish a MyRoom snapshot to a visitor"
        );
        let cached_owner = myroom_profile_presentation(current_profile).player_for(&identity);

        let invalid_club_name = "x".repeat(MAX_CLUB_NAME_UTF16_UNITS + 1);
        ProfileStore::new(profile_root.path())
            .update(&identity.nickname, |profile| {
                profile.rider.premium = 43;
                profile.rider.club_name.clone_from(&invalid_club_name);
            })
            .unwrap();
        let invalid_responses = handle_get_rider(&world, &profiles, session, &mut context)
            .await
            .unwrap();
        assert!(!invalid_responses.is_empty());
        let current = world.authorize_identity(session).await.unwrap();
        let invalid_profile = context.profile_for(&current).unwrap();
        assert_eq!(invalid_profile.rider.premium, 43);
        assert_eq!(invalid_profile.rider.club_name, invalid_club_name);
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        world.session_closed(visitor.session).await.unwrap();
        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create(&identity.nickname)
            .unwrap();
        assert_eq!(persisted.revision, Some(4));
        assert_eq!(persisted.profile.rider_item.kart_serial, 1);
        let expected_slots: [MyRoomSlot; MYROOM_SLOT_COUNT] = array::from_fn(|slot| {
            if slot == 0 {
                MyRoomSlot::Player(cached_owner.clone())
            } else {
                MyRoomSlot::Empty
            }
        });
        let packet = owner.outbound.try_recv().unwrap().into_packets();
        assert_eq!(packet, vec![serialize_slot_data(&expected_slots).unwrap()]);
        assert!(owner.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test]
    async fn creation_policy_rejects_unknown_profiles_without_allocating_disk_state() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);

        let admission = profiles
            .admit("RemoteRider", "test denied profile load")
            .await
            .unwrap();
        let error = profiles
            .load("RemoteRider".to_owned(), false, admission)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            LoginSessionError::ProfileCreationDenied { ref nickname }
                if nickname == "RemoteRider"
        ));
        assert!(!profile_root.path().join("RemoteRider").exists());

        fs::create_dir(profile_root.path().join("RemoteRider")).unwrap();
        let admission = profiles
            .admit("remoterider", "test existing profile load")
            .await
            .unwrap();
        let (loaded, lane) = profiles
            .load("remoterider".to_owned(), false, admission)
            .await
            .unwrap();
        assert!(loaded.source_path.is_file());
        drop(lane);
    }

    #[test]
    fn rider_selection_accepts_catalog_grants_and_preserves_existing_legacy_values() {
        let catalog = test_catalog();
        let mut profile = Profile::default();
        let selection = rider_selection();
        validate_rider_item_selection(&catalog, &profile, selection).unwrap();

        let mut invalid = selection;
        invalid.character = 999;
        assert!(matches!(
            validate_rider_item_selection(&catalog, &profile, invalid),
            Err(RiderEquipmentValidationError::RiderItemNotGranted {
                category: 1,
                item_id: 999
            })
        ));

        profile.rider_item.character = 999;
        validate_rider_item_selection(&catalog, &profile, invalid).unwrap();
        profile.granted_karts.push(GrantedKart {
            kart_id: 1,
            serial: 2,
        });
        let mut duplicate_kart = selection;
        duplicate_kart.kart_serial = 2;
        validate_rider_item_selection(&catalog, &profile, duplicate_kart).unwrap();
        duplicate_kart.kart_serial = 3;
        assert!(matches!(
            validate_rider_item_selection(&catalog, &profile, duplicate_kart),
            Err(RiderEquipmentValidationError::KartNotGranted {
                kart_id: 1,
                serial: 3
            })
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn migration_preflight_survives_source_disconnect_while_profile_lane_waits() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 49_900))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 49_901))
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "MigratingRider")
            .await
            .unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (_, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        drop(lane);

        let token = MigrationToken::new(0x5135).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let held_admission = profiles
            .admit(&identity.nickname, "hold migration profile lane")
            .await
            .unwrap();
        let preflight = world
            .preflight_migration(destination, identity.user_no, 12, token, Instant::now())
            .await
            .unwrap();

        let migration_profiles = profiles.clone();
        let migration_world = world.clone();
        let mut migration = tokio::spawn(async move {
            let admission = migration_profiles
                .admit(preflight.nickname(), "load migrated profile")
                .await
                .unwrap();
            let (profile, lane) = migration_profiles
                .load(preflight.nickname().to_owned(), false, admission)
                .await
                .unwrap();
            let presentation = myroom_profile_presentation(&profile.profile);
            let profile_lease = MyRoomProfileLease::new(presentation, lane);
            migration_world
                .complete_preflighted_migration(preflight, profile_lease)
                .await
                .unwrap()
        });

        assert!(
            time::timeout(Duration::from_millis(50), &mut migration)
                .await
                .is_err(),
            "migration unexpectedly bypassed the held profile lane"
        );
        world.session_closed(source).await.unwrap();
        drop(held_admission);

        let completion = migration.await.unwrap();
        assert_eq!(completion.previous_owner, None);
        assert_eq!(completion.binding.owner, destination);
        assert_eq!(
            completion.binding.channel.map(|channel| channel.game_type),
            Some(67)
        );

        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "the cancellation test exercises the complete handler, lane wait, exact abort, and successful retry lifecycle"
    )]
    async fn cancelled_channel_move_in_handler_releases_its_exact_migration_freeze() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 49_902))
            .await
            .unwrap();
        let (destination, _destination_cancelled, mut destination_outbound) = world
            .register_login_session(
                SocketAddr::new(address, 49_903),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "CancelledMigrationRider")
            .await
            .unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (_, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        drop(lane);

        let token = MigrationToken::new(0x5137).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let held_admission = profiles
            .admit(&identity.nickname, "hold cancelled migration profile lane")
            .await
            .unwrap();
        let mut packet = PacketWriter::named("PqChannelMovein");
        packet.write_u32(identity.user_no.get());
        packet.write_u16(12);
        packet.write_u16(token.get());
        let packet = packet.into_inner();

        let cancelled_world = world.clone();
        let cancelled_profiles = profiles.clone();
        let cancelled_packet = packet.clone();
        let mut handler = tokio::spawn(async move {
            let mut context = SessionContext::default();
            handle_channel_move_in(
                &ServerConfig::default(),
                &cancelled_world,
                &cancelled_profiles,
                destination,
                &cancelled_packet,
                &mut context,
            )
            .await
        });
        time::timeout(Duration::from_secs(1), async {
            loop {
                if matches!(
                    world.authorize_identity(source).await,
                    Err(WorldError::Identity(
                        IdentityError::TransferInProgress { .. }
                    ))
                ) {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the channel-move handler never installed its source freeze");
        assert!(
            time::timeout(Duration::from_millis(20), &mut handler)
                .await
                .is_err(),
            "the migration handler did not remain blocked on the held profile lane"
        );

        handler.abort();
        assert!(handler.await.unwrap_err().is_cancelled());
        world.drain_myroom_completions().await.unwrap();
        assert_eq!(world.authorize_identity(source).await.unwrap(), identity);
        assert!(matches!(
            world.authorize_identity(destination).await,
            Err(WorldError::Identity(
                IdentityError::UnauthenticatedSession(session)
            )) if session == destination
        ));

        drop(held_admission);
        let mut retry_context = SessionContext::default();
        let responses = handle_channel_move_in(
            &ServerConfig::default(),
            &world,
            &profiles,
            destination,
            &packet,
            &mut retry_context,
        )
        .await
        .unwrap();
        assert!(
            responses.is_empty(),
            "the migration acknowledgement is actor-ordered, not a direct response"
        );
        let acknowledgement = time::timeout(Duration::from_secs(1), destination_outbound.recv())
            .await
            .expect("the migration acknowledgement was not queued")
            .expect("the destination outbound channel closed")
            .into_packets();
        assert_eq!(
            acknowledgement,
            vec![serialize_pr_channel_move_in(
                ServerConfig::default().ports.game_udp(),
                ServerConfig::default().ports.p2p_udp(),
            )]
        );
        assert!(
            destination_outbound.try_recv().is_err(),
            "the migration acknowledgement must be queued exactly once"
        );
        let migrated = world.authorize_identity(destination).await.unwrap();
        assert_eq!(migrated.user_no, identity.user_no);
        assert!(migrated.generation.get() > identity.generation.get());
        assert!(retry_context.bound_profile_for(&migrated).is_ok());

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn migration_profile_failure_preserves_source_owner_permit_and_destination_fence() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(16).expect("nonzero World mailbox capacity");
        let remote = IpAddr::V4(Ipv4Addr::new(203, 0, 113, 9));
        let (source, mut source_cancelled, _source_outbound) = world
            .register_login_session(
                SocketAddr::new(remote, 49_910),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let (destination, _destination_cancelled, _destination_outbound) = world
            .register_login_session(
                SocketAddr::new(remote, 49_911),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "MissingMigrationProfile")
            .await
            .unwrap();
        let token = MigrationToken::new(0x5134).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let mut packet = PacketWriter::named("PqChannelMovein");
        packet.write_u32(identity.user_no.get());
        packet.write_u16(12);
        packet.write_u16(token.get());

        let error = handle_channel_move_in(
            &ServerConfig::default(),
            &world,
            &profiles,
            destination,
            &packet.into_inner(),
            &mut SessionContext::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            LoginSessionError::ProfileCreationDenied { ref nickname }
                if nickname == "MissingMigrationProfile"
        ));
        assert_eq!(world.authorize_identity(source).await.unwrap(), identity);
        assert!(matches!(
            world.authorize_identity(destination).await,
            Err(WorldError::Identity(
                IdentityError::UnauthenticatedSession(session)
            )) if session == destination
        ));
        assert!(
            time::timeout(Duration::from_millis(20), &mut source_cancelled)
                .await
                .is_err(),
            "profile failure must not cancel the still-authoritative source session"
        );
        let retry = world
            .preflight_migration(destination, identity.user_no, 12, token, Instant::now())
            .await
            .unwrap();
        assert_eq!(retry.source_generation(), identity.generation);
        assert!(matches!(
            world.authorize_identity(source).await,
            Err(WorldError::Identity(IdentityError::TransferInProgress {
                ref nickname,
            })) if nickname == "MissingMigrationProfile"
        ));
        drop(retry);
        world.drain_myroom_completions().await.unwrap();
        assert_eq!(world.authorize_identity(source).await.unwrap(), identity);

        world.session_closed(destination).await.unwrap();
        world.session_closed(source).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        profile_runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::too_many_lines,
        reason = "the cancellation regression spans profile worker, identity drain, migration commit, and durable verification"
    )]
    async fn cancelled_update_retains_identity_child_until_disk_save_finishes() {
        let profile_root = tempfile::tempdir().unwrap();
        let hook = BlockingUpdateHook::new();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let profiles = profiles.with_blocking_update_hook(Arc::clone(&hook));
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 50_000))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 50_001))
            .await
            .unwrap();
        let identity = world.claim_identity(source, "Rider").await.unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (_, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        drop(lane);

        let token = MigrationToken::new(0x5136).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        let update_profiles = profiles.clone();
        let update_nickname = identity.nickname.clone();
        let update_operation = world.admit_identity_operation(source).await.unwrap();
        let update = tokio::spawn(async move {
            let admission = update_profiles
                .admit_for_operation(
                    &update_operation,
                    &update_nickname,
                    "test blocked game-option update",
                )
                .await?;
            update_profiles
                .update_game_options(
                    update_nickname,
                    GameOptions {
                        video_quality: 77,
                        ..GameOptions::default()
                    },
                    admission,
                )
                .await
        });
        let entered_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || entered_hook.entered.wait())
            .await
            .unwrap();

        update.abort();
        assert!(update.await.unwrap_err().is_cancelled());

        let migration_profiles = profiles.clone();
        let migration_world = world.clone();
        let user_no = identity.user_no;
        let migration_nickname = identity.nickname.clone();
        let (attempting, attempted) = oneshot::channel();
        let mut migration = tokio::spawn(async move {
            let preflight = migration_world
                .preflight_migration(destination, user_no, 12, token, Instant::now())
                .await
                .unwrap();
            let _ = attempting.send(());
            preflight.wait_for_operations_drained().await.unwrap();
            let admission = migration_profiles
                .admit(&migration_nickname, "test migration handoff")
                .await
                .unwrap();
            let (profile, lane) = migration_profiles
                .load(migration_nickname, false, admission)
                .await
                .unwrap();
            let profile_lease =
                MyRoomProfileLease::new(myroom_profile_presentation(&profile.profile), lane);
            migration_world
                .complete_preflighted_migration(preflight, profile_lease)
                .await
        });
        attempted.await.unwrap();
        assert!(
            time::timeout(Duration::from_millis(50), &mut migration)
                .await
                .is_err(),
            "migration acquired the ownership gate while the cancelled save still ran"
        );

        let release_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || release_hook.release.wait())
            .await
            .unwrap();
        migration.await.unwrap().unwrap();

        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(persisted.revision, Some(2));
        assert_eq!(persisted.profile.game_option.video_quality, 77);

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
        profile_runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelled_plant_write_keeps_ownership_gate_until_atomic_publish_finishes() {
        let profile_root = tempfile::tempdir().unwrap();
        let hook = BlockingUpdateHook::new();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let profiles = profiles.with_blocking_update_hook(Arc::clone(&hook));
        let write_profiles = profiles.clone();
        let request = PlantPartEquipRequest {
            item_category: 43,
            item_id: 1_000,
            kart_category: 3,
            kart_id: 1,
            kart_serial: 1,
        };
        let write = tokio::spawn(async move {
            let admission = write_profiles
                .admit("PlantRider", "test blocked plant write")
                .await?;
            write_profiles.equip_plant_part(request, admission).await
        });
        let entered_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || entered_hook.entered.wait())
            .await
            .unwrap();
        write.abort();
        assert!(write.await.unwrap_err().is_cancelled());

        let gate_profiles = profiles.clone();
        let mut next_owner = tokio::spawn(async move {
            gate_profiles
                .admit("plantrider", "test next plant owner")
                .await
        });
        assert!(
            time::timeout(Duration::from_millis(50), &mut next_owner)
                .await
                .is_err(),
            "another owner acquired the gate while the cancelled plant write still ran"
        );
        let release_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || release_hook.release.wait())
            .await
            .unwrap();
        drop(next_owner.await.unwrap().unwrap());

        let equipment =
            EquipmentExceptions::load(profile_root.path(), profile_root.path().join("PlantRider"))
                .unwrap();
        assert_eq!(equipment.plant.len(), 1);
        assert_eq!(equipment.plant[0].id, 1);
        assert_eq!(equipment.plant[0].engine_category, 43);
        assert_eq!(equipment.plant[0].engine_id, 1_000);
    }

    #[tokio::test]
    async fn stale_generation_cannot_publish_a_profile_update() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 50_000))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 50_001))
            .await
            .unwrap();
        let identity = world.claim_identity(source, "Rider").await.unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial profile load")
            .await
            .unwrap();
        let (profile, lane) = profiles
            .load(identity.nickname.clone(), true, admission)
            .await
            .unwrap();
        let mut context = SessionContext::default();
        context.bind_profile(identity.clone(), profile);
        drop(lane);

        let token = MigrationToken::new(0x5136).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 12,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        world
            .complete_migration(destination, identity.user_no, 12, token, Instant::now())
            .await
            .unwrap();

        let mut update = PacketWriter::named("PqUpdateGameOption");
        update.write_f32(0.25);
        update.write_f32(0.5);
        update.write_bytes(&[99; 27]);
        assert!(matches!(
            update_game_options(
                &world,
                &profiles,
                source,
                update.as_slice(),
                &mut context
            )
            .await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));

        let persisted = ProfileStore::new(profile_root.path())
            .load_or_create("Rider")
            .unwrap();
        assert_eq!(persisted.revision, Some(1));
        assert_eq!(persisted.profile.game_option.video_quality, 14);

        world.session_closed(source).await.unwrap();
        world.session_closed(destination).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }
}
