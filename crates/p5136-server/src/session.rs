use std::{
    array, future::Future, io, mem::size_of, net::SocketAddr, path::PathBuf, pin::Pin, sync::Arc,
    time::Instant,
};

use chrono::{Local, NaiveDate, Timelike};
use p5136_core::{
    adler32,
    captured_query_protocol::{
        CapturedQueryError, CapturedQueryRequest, classify_captured_query_request,
        process_captured_query_request,
    },
    channel::{
        ChannelError, ClientEndpointReportKind, classify_client_endpoint_report,
        parse_client_endpoint_report, parse_pq_channel_movein, parse_pq_channel_switch,
        resolve_channel_id, serialize_pr_channel_move_in, serialize_pr_channel_switch,
    },
    client_event_protocol::{
        ClientEvent, ClientEventProtocolError, ClientEventRequest, classify_client_event,
        parse_client_event,
    },
    club_query_protocol::{
        ClubQueryProtocolError, ClubQueryRequest, classify_club_query_request,
        parse_club_query_request, serialize_club_creation_unavailable_reply,
        serialize_empty_club_list_count_reply, serialize_no_club_state_reply,
        serialize_no_pending_club_join_reply, serialize_unavailable_waiting_crew_count_reply,
    },
    equipment_protocol::{
        EquipmentProtocolError, EquipmentRequest, PlantPartEquipRequest, RiderItemSelection,
        XPartEquipRequest, classify_equipment_request, parse_equip_plant_part, parse_equip_x_part,
        parse_set_rider_items, serialize_equip_tuning_failure, serialize_equip_tuning_success,
        serialize_equip_x_part_failure, serialize_equip_x_part_success,
    },
    frame::{self, FrameError},
    game_slot_protocol::parse_game_slot_packet,
    handshake,
    inventory::{InventoryError, serialize_get_rider_sequence},
    item_state_protocol::{
        DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, ItemStateProtocolError, ItemStateRequest,
        classify_item_state_request, favorite_item_list_capacity, parse_item_state_request,
        serialize_favorite_item_list, serialize_locked_item_list,
    },
    kart_physics::{
        KartPhysicsBuildError, P5136KartPhysicsSnapshot, build_p5136_kart_physics_block,
    },
    lobby_protocol::{
        LobbyProtocolError, LobbyRequest, classify_lobby_request, parse_basic_ai_request,
        parse_change_master_request, parse_change_team_request, parse_change_track_request,
        parse_close_slot_request, parse_macro_chat_request, parse_rider_talk_request,
        parse_set_slot_state_request, parse_start_room_request,
    },
    login::{
        LegacyTime, LoginError, PrLoginFields, parse_pq_login, serialize_pr_cn_authen_login,
        serialize_pr_login,
    },
    myroom_protocol::{
        EnterMyRoomRequest, MYROOM_ITEM_CHUNK_SIZE, MyRoomInfo, MyRoomPlayerSlot,
        MyRoomProtocolError, MyRoomRequest, classify_myroom_request, parse_character_position,
        parse_check_password, parse_enter_random_request, parse_enter_request, parse_first_request,
        parse_reenter_request, parse_request_career_list, parse_request_emblems,
        parse_request_items, parse_rider_talk, parse_secede_request, parse_update_info,
        parse_update_main_emblem, plan_owner_item_packets, serialize_check_password_reply,
        serialize_myroom_info, serialize_owner_emblems, serialize_owner_item_enchants,
        serialize_owner_items, serialize_update_main_emblem_reply,
    },
    nickname::canonical_nickname_key,
    packet::PacketError,
    race_protocol::{
        RaceProtocolError, RaceRequest, classify_race_request, parse_ai_goal_in_request,
        parse_game_control_request, parse_team_booster_request,
    },
    race_start_protocol::P5136KartPhysicsBlock,
    rider_info_protocol::{
        GET_RIDER_INFO_REQUEST_HASH, RiderInfoProtocolError, parse_get_rider_info_request,
        serialize_get_rider_info_failure,
    },
    room_protocol::{
        RoomAi, RoomPlayer, RoomProtocolError, RoomProtocolRequest, classify_room_protocol_request,
        parse_ch_create_room_request, parse_ch_get_room_list_request, parse_ch_join_room_request,
        parse_ch_leave_room_request, parse_gr_first_request,
    },
    scenario_protocol::{
        ScenarioProtocolError, ScenarioRequest, classify_scenario_request,
        parse_complete_scenario_request, parse_start_scenario_request,
        serialize_complete_scenario_reply, serialize_start_scenario_reply,
    },
    shop_protocol::{
        ShopProtocolError, classify_shop_buy_request, parse_shop_buy_request,
        serialize_shop_buy_failure,
    },
    single_player_protocol::{
        FinishTimeAttackRequest, SinglePlayerProtocolError, SinglePlayerRequest,
        SinglePlayerRequestKind, StartTimeAttackRequest, classify_single_player_request,
        parse_single_player_request, serialize_finish_time_attack_reply, serialize_kart_spec_reply,
        serialize_start_time_attack_reply,
    },
    startup::{
        self, PrGetRiderFields, RIDER_ITEM_SNAPSHOT_WIRE_LENGTH, StartupError, StartupRequest,
        classify_startup_request, is_startup_noop, parse_pq_favorite_track_map_get,
        parse_pq_get_rider_task_context, parse_pq_locked_item_get, parse_pq_ranker_info,
        parse_pq_request_extradata, parse_pq_rider_school_expired_check,
        parse_pq_start_rider_school, parse_pq_update_game_option, parse_pq_versus_mode_rank_one,
        parse_pq_web_event_complete_check, parse_sp_rq_get_cash_inventory,
        parse_sp_rq_get_max_gift_id, parse_sp_rq_koin_balance, parse_sp_rq_remain_cash,
        parse_sp_rq_remain_tc_cash,
    },
    telemetry_protocol::{
        TelemetryProtocolError, TelemetryReport, TelemetryRequestKind, classify_telemetry_request,
        parse_telemetry_request,
    },
    track::P5136_FALLBACK_TRACK_ID,
};
use p5136_profile::{
    AppliedTimeReward, CatalogInventory, DEFAULT_RP, EmblemCatalog, EquipmentExceptions,
    EquipmentProfileError, EquipmentStateError, InventoryBuildError, MAX_MYROOM_ITEM_RECORDS,
    MyRoomItemStateError, MyRoomOwnerInventory, Profile, ProfileMutation, ProfileStoreError,
    ProfileTransaction, build_inventory_snapshot_with_equipment, finish_reward,
    generated_x_part_is_granted, rider_item_snapshot,
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
    favorite_persistence::{
        DurableFavoriteItems, FAVORITE_ITEM_UPDATE_OPERATION, FavoriteItemPersistError,
        persist_favorite_item_changes,
    },
    identity::{IdentityOperationLease, legacy_p2p_endpoint},
    locked_item_persistence::{
        DurableLockedItems, LOCKED_ITEM_UPDATE_OPERATION, LockedItemPersistError,
        persist_locked_item_changes,
    },
    main_emblem_persistence::{
        MAIN_EMBLEM_WRITE_OPERATION, MainEmblemPublication, MainEmblemWriteError,
        MainEmblemWriteReceipt, PreparedMainEmblemWrite, ValidatedMainEmblemSelection,
    },
    myroom_hub::{MyRoomWirePlan, MyRoomWireProjection},
    myroom_persistence::{
        MYROOM_INFO_WRITE_OPERATION, MyRoomCompletionSlot, MyRoomInfoWriteError,
        MyRoomInfoWriteReceipt,
    },
    operation_gate::{WireOperationGate, WireOperationGuard},
    packet_log::{PacketDirection, SessionFailure, trace_packet, trace_session_failure},
    profile_io::{
        MyRoomProfileLease, ProfileIoError, ProfileIoHandle, ProfileJobAdmission,
        ProfileLanePermit, myroom_profile_presentation,
    },
    profile_presentation_persistence::{
        PROFILE_PRESENTATION_WRITE_OPERATION, PreparedProfilePresentationWrite,
        ProfilePresentationMutation, ProfilePresentationWriteError,
        ProfilePresentationWriteReceipt,
    },
    world::{
        AdmittedWorldHandle, LobbyCommandPayload, LobbyError, MyRoomCommandPayload,
        MyRoomEntryInput, MyRoomOwnerItemLoad, MyRoomPeerCommandPayload, MyRoomSessionRole,
        OutboundBatch, RaceCommandOutcome, RaceCommandPayload, RoomCommandPayload, RoomParticipant,
        StartRoomPlan,
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
    CapturedQueryProtocol(#[from] CapturedQueryError),

    #[error(transparent)]
    ClientEventProtocol(#[from] ClientEventProtocolError),

    #[error(transparent)]
    SinglePlayerProtocol(#[from] SinglePlayerProtocolError),

    #[error(transparent)]
    TelemetryProtocol(#[from] TelemetryProtocolError),

    #[error(transparent)]
    ProfileStore(#[from] ProfileStoreError),

    #[error(transparent)]
    EquipmentState(#[from] EquipmentStateError),

    #[error(transparent)]
    EquipmentProfile(#[from] EquipmentProfileError),

    #[error(transparent)]
    InventoryBuild(#[from] InventoryBuildError),

    #[error(transparent)]
    InventoryProtocol(#[from] InventoryError),

    #[error(transparent)]
    RoomProtocol(#[from] RoomProtocolError),

    #[error(transparent)]
    ScenarioProtocol(#[from] ScenarioProtocolError),

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
    ShopProtocol(#[from] ShopProtocolError),

    #[error(transparent)]
    RiderInfoProtocol(#[from] RiderInfoProtocolError),

    #[error(transparent)]
    ClubQueryProtocol(#[from] ClubQueryProtocolError),

    #[error(transparent)]
    ItemStateProtocol(#[from] ItemStateProtocolError),

    #[error(transparent)]
    MyRoomItemState(#[from] MyRoomItemStateError),

    #[error("live MyRoom wire projection failed")]
    MyRoomWireProjection {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("MyRoom entry profile projection failed")]
    MyRoomEntryPreparation {
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

    #[error("main-emblem persistence or publication failed")]
    MainEmblemWrite {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error("profile-presentation persistence or publication failed")]
    ProfilePresentationWrite {
        #[source]
        source: Box<dyn std::error::Error + Send + Sync + 'static>,
    },

    #[error(transparent)]
    FavoriteItemPersistence(#[from] FavoriteItemPersistError),

    #[error(transparent)]
    LockedItemPersistence(#[from] LockedItemPersistError),

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

    #[error("the World actor returned a non-GameSlot outcome for a TCP GameSlot command")]
    UnexpectedGameSlotOutcome,

    #[error("client did not complete login before the login timeout")]
    LoginTimeout,

    #[error("authenticated login session exceeded its idle timeout")]
    SessionIdleTimeout,

    #[error("login session write exceeded its timeout")]
    WriteTimeout,

    #[error("logical login packet is shorter than its four-byte name hash")]
    MissingPacketHash,

    #[error("unsupported identity-bound packet hash 0x{hash:08X}")]
    UnsupportedIdentityPacket { hash: u32 },

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

    #[error(
        "time-attack entry requires {required} Lucci but the current profile has only {available}"
    )]
    TimeAttackInsufficientLucci { required: u32, available: u32 },

    #[error("PqFinishTimeAttack arrived without an active, unfinished time-attack start")]
    TimeAttackFinishWithoutStart,

    #[error("PqStartTimeAttack arrived while the session already has an unfinished time attack")]
    TimeAttackStartWhileActive,

    #[error("time-attack Lucci reward overflowed the profile balance")]
    TimeAttackLucciOverflow,

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
    emblems: Option<Arc<EmblemCatalog>>,
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
    #[cfg(test)]
    pub(crate) fn new(io: ProfileIoHandle, catalog: Option<Arc<CatalogInventory>>) -> Self {
        Self::new_with_emblems(io, catalog, None)
    }

    #[must_use]
    pub(crate) fn new_with_emblems(
        io: ProfileIoHandle,
        catalog: Option<Arc<CatalogInventory>>,
        emblems: Option<Arc<EmblemCatalog>>,
    ) -> Self {
        let emblems = emblems.or_else(|| {
            catalog
                .as_deref()
                .and_then(CatalogInventory::emblem_definitions)
                .cloned()
                .map(Arc::new)
        });
        Self {
            io,
            catalog,
            emblems,
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

    fn emblem_catalog(&self) -> Option<&EmblemCatalog> {
        self.emblems.as_deref()
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

    async fn update_scenario_type(
        &self,
        nickname: String,
        scenario_type: i32,
        admission: ProfileJobAdmission,
    ) -> Result<(ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run("update scenario type", move |store, _, subject| {
                #[cfg(test)]
                if let Some(hook) = blocking_update_hook {
                    hook.entered.wait();
                    hook.release.wait();
                }
                store.update(subject.nickname(), |profile| {
                    profile.rider.scenario_type = scenario_type;
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

    async fn start_time_attack(
        &self,
        nickname: String,
        request: StartTimeAttackRequest,
        track: u32,
        admission: ProfileJobAdmission,
    ) -> Result<(ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        let completed = admission
            .run("start time attack", move |store, _, subject| {
                store.transaction(subject.nickname(), |profile| {
                    let entry_fee = request.entry_fee();
                    if profile.rider.lucci < entry_fee {
                        return ProfileMutation::Unchanged(Err(
                            LoginSessionError::TimeAttackInsufficientLucci {
                                required: entry_fee,
                                available: profile.rider.lucci,
                            },
                        ));
                    }
                    let mut next = profile.clone();
                    next.rider.lucci -= entry_fee;
                    next.rider.speed_type = request.speed_type;
                    next.rider.game_type = request.game_type;
                    next.rider.attack_type = request.attack_type;
                    next.rider.track = track;
                    ProfileMutation::changed(Ok(()), next)
                })
            })
            .await?;
        let (transaction, lane) = completed.into_parts();
        let ((), profile) = committed_profile_transaction(transaction?)?;
        Ok((profile, lane))
    }

    async fn finish_time_attack(
        &self,
        nickname: String,
        request: FinishTimeAttackRequest,
        admission: ProfileJobAdmission,
    ) -> Result<(AppliedTimeReward, ProfileSnapshot, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        let reward = finish_reward(request.reward_type);
        let completed = admission
            .run("finish time attack", move |store, _, subject| {
                store.transaction(subject.nickname(), |profile| {
                    let Some(current_lucci) =
                        profile.rider.lucci.checked_add(reward.earned_lucci())
                    else {
                        return ProfileMutation::Unchanged(Err(
                            LoginSessionError::TimeAttackLucciOverflow,
                        ));
                    };
                    let mut next = profile.clone();
                    next.rider.time = request.race_time;
                    next.rider.rp = DEFAULT_RP;
                    next.rider.lucci = current_lucci;
                    let applied = AppliedTimeReward {
                        current_rp: DEFAULT_RP,
                        earned_rp: reward.earned_rp(),
                        earned_lucci: reward.earned_lucci(),
                        current_lucci,
                    };
                    ProfileMutation::changed(Ok(applied), next)
                })
            })
            .await?;
        let (transaction, lane) = completed.into_parts();
        let (applied, profile) = committed_profile_transaction(transaction?)?;
        Ok((applied, profile, lane))
    }

    async fn update_favorite_items(
        &self,
        nickname: String,
        changes: Vec<p5136_core::item_state_protocol::FavoriteItemChange>,
        maximum_records: usize,
        admission: ProfileJobAdmission,
    ) -> Result<(DurableFavoriteItems, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run(
                FAVORITE_ITEM_UPDATE_OPERATION,
                move |store, lease, subject| {
                    #[cfg(test)]
                    if let Some(hook) = blocking_update_hook {
                        hook.entered.wait();
                        hook.release.wait();
                    }
                    persist_favorite_item_changes(
                        store,
                        lease,
                        subject.nickname(),
                        &changes,
                        maximum_records,
                    )
                },
            )
            .await?;
        let (updated, lane) = completed.into_parts();
        Ok((updated?, lane))
    }

    async fn update_locked_items(
        &self,
        nickname: String,
        changes: Vec<p5136_core::item_state_protocol::FavoriteItemChange>,
        maximum_records: usize,
        admission: ProfileJobAdmission,
    ) -> Result<(DurableLockedItems, ProfileLanePermit), LoginSessionError> {
        Self::ensure_admitted_subject(&admission, &nickname)?;
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run(
                LOCKED_ITEM_UPDATE_OPERATION,
                move |store, lease, subject| {
                    #[cfg(test)]
                    if let Some(hook) = blocking_update_hook {
                        hook.entered.wait();
                        hook.release.wait();
                    }
                    persist_locked_item_changes(
                        store,
                        lease,
                        subject.nickname(),
                        &changes,
                        maximum_records,
                    )
                },
            )
            .await?;
        let (updated, lane) = completed.into_parts();
        Ok((updated?, lane))
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

    fn validate_main_emblems(
        &self,
        request: p5136_core::myroom_protocol::UpdateMainEmblemRequest,
    ) -> Result<
        ValidatedMainEmblemSelection,
        crate::main_emblem_persistence::MainEmblemValidationError,
    > {
        ValidatedMainEmblemSelection::validate(request, self.emblem_catalog())
    }

    #[cfg_attr(
        not(test),
        expect(
            clippy::unused_self,
            reason = "test builds attach the shared blocking profile-write hook here"
        )
    )]
    fn prepare_main_emblem_write(
        &self,
        selection: ValidatedMainEmblemSelection,
        admission: ProfileJobAdmission,
        completion: MyRoomCompletionSlot,
    ) -> PreparedMainEmblemWrite {
        let prepared = PreparedMainEmblemWrite::new(admission, selection, completion);
        #[cfg(test)]
        let prepared = if let Some(hook) = self.blocking_update_hook.clone() {
            prepared.with_test_hook(Arc::new(move || {
                hook.entered.wait();
                hook.release.wait();
            }))
        } else {
            prepared
        };
        prepared
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

    async fn equip_x_part(
        &self,
        request: XPartEquipRequest,
        admission: ProfileJobAdmission,
    ) -> Result<(bool, ProfileLanePermit), LoginSessionError> {
        let catalog = self.catalog.clone();
        #[cfg(test)]
        let blocking_update_hook = self.blocking_update_hook.clone();
        let completed = admission
            .run("equip X-part", move |store, lease, subject| {
                #[cfg(test)]
                if let Some(hook) = blocking_update_hook {
                    hook.entered.wait();
                    hook.release.wait();
                }
                let loaded = store.load_or_create(subject.nickname())?;
                if !x_part_is_owned(catalog.as_deref(), &loaded.profile, request) {
                    return Ok::<_, LoginSessionError>(false);
                }
                store.equip_x_part(lease, subject.nickname(), request)?;
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
            .run(
                "load fresh rider inventory",
                move |store, lease, subject| {
                    let mut loaded = store.load_or_create(subject.nickname())?;
                    let equipped_kart = loaded.profile.rider_item.kart;
                    let equipped_kart_is_ungranted =
                        equipped_kart != 0 && !catalog.grants_item(3, equipped_kart);
                    if equipped_kart_is_ungranted
                        || (equipped_kart != 0 && loaded.profile.rider_item.kart_serial == 0)
                    {
                        let (saved, profile) = store.update(subject.nickname(), |profile| {
                            if profile.rider_item.kart != 0
                                && !catalog.grants_item(3, profile.rider_item.kart)
                            {
                                profile.rider_item.kart = 0;
                                profile.rider_item.kart_serial = 0;
                            } else if profile.rider_item.kart != 0
                                && profile.rider_item.kart_serial == 0
                            {
                                profile.rider_item.kart_serial = 1;
                            }
                        })?;
                        loaded.profile = profile;
                        loaded.revision = Some(saved.revision);
                        loaded.source_path = saved.path;
                    }
                    let equipment = store.load_equipment_exceptions(lease, subject.nickname())?;
                    let inventory = build_inventory_snapshot_with_equipment(
                        &catalog,
                        &loaded.profile,
                        equipment,
                    )?;
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
                },
            )
            .await?;
        let (result, lane) = completed.into_parts();
        let (responses, profile) = result?;
        Ok((responses, profile, lane))
    }
}

fn committed_profile_transaction<T>(
    transaction: ProfileTransaction<Result<T, LoginSessionError>>,
) -> Result<(T, ProfileSnapshot), LoginSessionError> {
    match transaction {
        ProfileTransaction::Committed {
            value,
            profile,
            saved,
        } => Ok((
            value?,
            ProfileSnapshot {
                profile,
                revision: Some(saved.revision),
                source_path: saved.path,
            },
        )),
        ProfileTransaction::Unchanged { value, .. } => match value {
            Err(error) => Err(error),
            Ok(_) => Err(ProfileStoreError::InternalInvariant {
                message: "a successful time-attack mutation must publish a profile revision",
            }
            .into()),
        },
        ProfileTransaction::CommittedButDurabilityUncertain { error, .. } => Err(error.into()),
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
    let Ok(mut kart_serial) = u16::try_from(request.kart_serial) else {
        return false;
    };
    if kart_id != 0 && kart_serial == 0 {
        kart_serial = 1;
    }
    if !kart_is_owned(catalog, profile, kart_id, kart_serial) {
        return false;
    }
    request.item_id == 0
        || u16::try_from(request.item_category)
            .ok()
            .zip(u16::try_from(request.item_id).ok())
            .is_some_and(|(category, item_id)| catalog_grants(catalog, category, item_id))
}

fn x_part_is_owned(
    catalog: Option<&CatalogInventory>,
    profile: &Profile,
    request: XPartEquipRequest,
) -> bool {
    let Some(catalog) = catalog else {
        return false;
    };
    let Ok(kart_id) = u16::try_from(request.kart_id) else {
        return false;
    };
    let Ok(mut kart_serial) = u16::try_from(request.kart_serial) else {
        return false;
    };
    if kart_id != 0 && kart_serial == 0 {
        kart_serial = 1;
    }
    if !kart_is_owned(catalog, profile, kart_id, kart_serial) {
        return false;
    }
    if request.item_id == 0 {
        return true;
    }
    let Ok(category) = u16::try_from(request.item_category) else {
        return false;
    };
    match category {
        63..=66 => generated_x_part_is_granted(profile.rider.slot_changer, request),
        68 | 69 => u16::try_from(request.item_id)
            .ok()
            .is_some_and(|item_id| catalog_grants(catalog, category, item_id)),
        _ => false,
    }
}

#[derive(Debug, Clone)]
struct ProfileSnapshot {
    profile: Profile,
    revision: Option<u64>,
    source_path: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveTimeAttack {
    request: StartTimeAttackRequest,
    track: u32,
}

#[derive(Debug, Default)]
struct SessionContext {
    profile: Option<BoundProfile>,
    active_time_attack: Option<ActiveTimeAttack>,
}

impl SessionContext {
    fn is_authenticated(&self) -> bool {
        self.profile.is_some()
    }

    fn bind_profile(&mut self, identity: IdentityBinding, profile: ProfileSnapshot) {
        // A persisted P2P port is historical profile data, not proof that this
        // exact TCP generation owns the same reachable endpoint. Preserve a
        // port only across reloads of the exact binding; every newly claimed
        // or migrated generation starts unadvertised until its own report is
        // durably accepted.
        let binding_unchanged = self
            .profile
            .as_ref()
            .filter(|bound| bound.identity == identity)
            .is_some();
        let reported_p2p_port = self
            .profile
            .as_ref()
            .filter(|bound| bound.identity == identity)
            .map_or(0, |bound| bound.reported_p2p_port);
        if !binding_unchanged {
            self.active_time_attack = None;
        }
        tracing::trace!(
            nickname = %identity.nickname,
            revision = ?profile.revision,
            source_path = %profile.source_path.display(),
            reported_p2p_port,
            "binding profile snapshot to session generation"
        );
        self.profile = Some(BoundProfile {
            identity,
            profile,
            reported_p2p_port,
        });
    }

    fn begin_time_attack(&mut self, request: StartTimeAttackRequest, track: u32) {
        self.active_time_attack = Some(ActiveTimeAttack { request, track });
    }

    fn ensure_time_attack_idle(&self) -> Result<(), LoginSessionError> {
        if self.active_time_attack.is_none() {
            Ok(())
        } else {
            Err(LoginSessionError::TimeAttackStartWhileActive)
        }
    }

    fn active_time_attack(&self) -> Result<ActiveTimeAttack, LoginSessionError> {
        self.active_time_attack
            .ok_or(LoginSessionError::TimeAttackFinishWithoutStart)
    }

    fn complete_time_attack(&mut self) {
        self.active_time_attack = None;
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

    fn reported_p2p_port_for(&self, identity: &IdentityBinding) -> Result<u16, LoginSessionError> {
        Ok(self.bound_profile_for(identity)?.reported_p2p_port)
    }

    fn myroom_presentation_for(
        &self,
        identity: &IdentityBinding,
    ) -> Result<crate::myroom_hub::MyRoomProfilePresentation, LoginSessionError> {
        let bound = self.bound_profile_for(identity)?;
        Ok(myroom_profile_presentation(&bound.profile.profile)
            .with_p2p_port(bound.reported_p2p_port))
    }

    fn room_participant_for(
        &self,
        identity: &IdentityBinding,
        catalog: Option<&CatalogInventory>,
    ) -> Result<RoomParticipant, LoginSessionError> {
        let bound = self.bound_profile_for(identity)?;
        room_participant_from_profile_with_p2p_port(
            identity,
            &bound.profile.profile,
            bound.reported_p2p_port,
            catalog,
        )
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

    fn apply_main_emblem_write(
        &mut self,
        identity: &IdentityBinding,
        receipt: &MainEmblemWriteReceipt,
    ) -> Result<(), LoginSessionError> {
        let bound = self
            .profile
            .as_mut()
            .filter(|bound| bound.identity.owner == identity.owner)
            .filter(|bound| bound.identity.user_no == identity.user_no)
            .filter(|bound| bound.identity.generation == identity.generation)
            .ok_or(LoginSessionError::ProfileNotBound)?;
        let [emblem_1, emblem_2, emblem_3] = receipt.selection().values();
        bound
            .profile
            .profile
            .rider
            .set_main_emblems(emblem_1, emblem_2, emblem_3);
        if let Some(revision) = receipt.revision() {
            bound.profile.revision = Some(revision);
        }
        Ok(())
    }

    fn apply_profile_presentation_write(
        &mut self,
        identity: &IdentityBinding,
        receipt: &ProfilePresentationWriteReceipt,
    ) -> Result<(), LoginSessionError> {
        let bound = self
            .profile
            .as_mut()
            .filter(|bound| &bound.identity == identity)
            .ok_or(LoginSessionError::ProfileNotBound)?;
        match receipt.mutation() {
            ProfilePresentationMutation::SetP2pPort(port) => {
                bound.reported_p2p_port = port;
                bound.profile.profile.rider.p2p_port = i32::from(port);
            }
        }
        bound.profile.revision = Some(receipt.revision());
        Ok(())
    }

    fn apply_favorite_item_write(
        &mut self,
        identity: &IdentityBinding,
        receipt: &DurableFavoriteItems,
    ) -> Result<(), LoginSessionError> {
        let bound = self
            .profile
            .as_mut()
            .filter(|bound| &bound.identity == identity)
            .ok_or(LoginSessionError::ProfileNotBound)?;
        bound.profile.profile.favorite_items = Some(receipt.items().clone());
        bound.profile.revision = Some(receipt.revision());
        Ok(())
    }

    fn apply_locked_item_write(
        &mut self,
        identity: &IdentityBinding,
        receipt: &DurableLockedItems,
    ) -> Result<(), LoginSessionError> {
        let bound = self
            .profile
            .as_mut()
            .filter(|bound| &bound.identity == identity)
            .ok_or(LoginSessionError::ProfileNotBound)?;
        bound.profile.profile.locked_items = Some(receipt.items().clone());
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
    reported_p2p_port: u16,
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

/// Reads one encrypted login frame while retaining malformed or partial wire
/// bytes in the local diagnostic sink. The public reader remains uninstrumented
/// for protocol fixtures; a live session uses this peer-aware boundary.
async fn read_encrypted_frame_with_diagnostics<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
    peer: Option<SocketAddr>,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    let mut header_read = 0;
    if let Err(error) = read_exact_with_count(reader, &mut header, &mut header_read).await {
        if header_read != 0 {
            trace_packet(
                "login-tcp",
                "partial-wire",
                PacketDirection::Received,
                peer,
                &header[..header_read],
            );
        }
        return Err(error.into());
    }
    let encoded_header = u32::from_le_bytes(header);
    let body_length = match frame::encrypted_body_length(encoded_header, *iv, maximum) {
        Ok(length) => length,
        Err(error) => {
            trace_packet(
                "login-tcp",
                "wire",
                PacketDirection::Received,
                peer,
                &header,
            );
            return Err(error.into());
        }
    };

    let mut wire = Vec::with_capacity(body_length + 4);
    wire.extend_from_slice(&header);
    wire.resize(body_length + 4, 0);
    let mut body_read = 0;
    if let Err(error) = read_exact_with_count(reader, &mut wire[4..], &mut body_read).await {
        trace_packet(
            "login-tcp",
            "partial-wire",
            PacketDirection::Received,
            peer,
            &wire[..4 + body_read],
        );
        return Err(error.into());
    }
    match frame::decode_encrypted(&wire, iv, maximum) {
        Ok(packet) => Ok(packet),
        Err(error) => {
            trace_packet("login-tcp", "wire", PacketDirection::Received, peer, &wire);
            Err(error.into())
        }
    }
}

async fn read_exact_with_count<R>(
    reader: &mut R,
    buffer: &mut [u8],
    read: &mut usize,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
{
    while *read < buffer.len() {
        let count = reader.read(&mut buffer[*read..]).await?;
        if count == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        *read += count;
    }
    Ok(())
}

async fn read_session_frame<R>(
    reader: &mut R,
    iv: &mut u32,
    maximum: usize,
    authenticated: bool,
    login_deadline: time::Instant,
    idle_timeout: std::time::Duration,
    peer: Option<SocketAddr>,
) -> Result<Vec<u8>, LoginSessionError>
where
    R: AsyncRead + Unpin,
{
    if authenticated {
        time::timeout(
            idle_timeout,
            read_encrypted_frame_with_diagnostics(reader, iv, maximum, peer),
        )
        .await
        .map_err(|_| LoginSessionError::SessionIdleTimeout)?
    } else {
        time::timeout_at(
            login_deadline,
            read_encrypted_frame_with_diagnostics(reader, iv, maximum, peer),
        )
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
    trace_packet(
        "login-tcp",
        "logical",
        PacketDirection::Sent,
        peer,
        &payload,
    );

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
            peer,
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
                        peer,
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
                            peer,
                        ) => result?,
                    }
                    outbound_burst += 1;
                }
                SessionReadEvent::Frame(result) => break result?,
            }
        };

        trace_packet(
            "login-tcp",
            "logical",
            PacketDirection::Received,
            peer,
            &packet,
        );

        let Some(wire_operation) = wire_operations.try_begin_request() else {
            return drain_outbound_until_cancelled(
                &mut writer,
                cancellation,
                outbound,
                &mut send_iv,
                config,
                peer,
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
                peer,
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
                    peer,
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
    peer: Option<SocketAddr>,
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
                    result = write_outbound_batch(writer, batch, send_iv, config, peer) => result?,
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
    peer: Option<SocketAddr>,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    // The raw packet has already been recorded at the transport boundary. Keep
    // the request hash beside a typed handler error so a stock-client failure
    // can be diagnosed without enabling a global debug filter.
    let request_hash =
        packet_hash(packet).map_or_else(|_| "<missing>".to_owned(), |hash| format!("0x{hash:08X}"));
    let responses =
        match dispatch_packet_admitted(services, packet, context, operation.identity()).await {
            Ok(responses) => responses,
            Err(error) => {
                trace_session_failure(
                    SessionFailure {
                        transport: "login-tcp",
                        peer,
                        stage: "request handler",
                        request_hash: Some(&request_hash),
                        request_bytes: Some(packet.len()),
                        response_count: None,
                        authenticated: Some(context.is_authenticated()),
                    },
                    &error,
                );
                return Err(error);
            }
        };
    let response_count = responses.len();
    if let Err(error) =
        write_logical_packets(writer, &responses, send_iv, services.config, peer).await
    {
        trace_session_failure(
            SessionFailure {
                transport: "login-tcp",
                peer,
                stage: "response write",
                request_hash: Some(&request_hash),
                request_bytes: Some(packet.len()),
                response_count: Some(response_count),
                authenticated: Some(context.is_authenticated()),
            },
            &error,
        );
        return Err(error);
    }
    Ok(())
}

async fn write_outbound_batch<W>(
    writer: &mut W,
    batch: OutboundBatch,
    send_iv: &mut u32,
    config: &ServerConfig,
    peer: Option<SocketAddr>,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    let (packets, _operation) = batch.into_write_parts();
    write_logical_packets(writer, &packets, send_iv, config, peer).await
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
    peer: Option<SocketAddr>,
) -> Result<(), LoginSessionError>
where
    W: AsyncWrite + Unpin,
{
    time::timeout(config.session_write_timeout, async {
        for packet in packets {
            let wire = frame::encode_encrypted(packet, send_iv, config.max_login_payload)?;
            writer.write_all(&wire).await?;
            trace_packet("login-tcp", "logical", PacketDirection::Sent, peer, packet);
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

    if let Some(result) = dispatch_fail_closed_request(&world, hash, packet, context).await {
        return result;
    }

    if hash == adler32::packet_hash("PqChannelSwitch") {
        return handle_channel_switch(services.config, &world, packet).await;
    }

    if let Some(kind) = classify_client_endpoint_report(hash) {
        return handle_client_endpoint_report(&world, services.profiles, kind, packet, context)
            .await;
    }

    if let Some(request) = classify_client_event(hash) {
        return handle_client_event(&world, request, packet, context).await;
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
        return handle_lobby_request_admitted(
            &world,
            request,
            packet,
            Some(context),
            services.profiles.catalog(),
        )
        .await;
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

    if let Some(result) =
        dispatch_profile_bound_request(services, &world, hash, packet, context).await
    {
        return result;
    }

    if let Some(request) = classify_captured_query_request(hash) {
        return handle_captured_query_request(&world, request, packet, context).await;
    }

    if is_startup_noop(hash) {
        return handle_startup_noop(&world, context).await;
    }

    // Identity-bound packets cannot be processed by a stale connection.
    // Unknown packets fail explicitly instead of impersonating a successful
    // no-reply handler. Deliberate compatibility no-ops remain classified by
    // `is_startup_noop` above.
    reject_unsupported_identity_packet(&world, hash, context).await
}

async fn reject_unsupported_identity_packet(
    world: &AdmittedWorldHandle<'_>,
    hash: u32,
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let identity = world.authorize_identity().await?;
    let _ = context.profile_for(&identity)?;
    Err(LoginSessionError::UnsupportedIdentityPacket { hash })
}

async fn handle_startup_noop(
    world: &AdmittedWorldHandle<'_>,
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let identity = world.authorize_identity().await?;
    let _ = context.profile_for(&identity)?;
    Ok(Vec::new())
}

async fn handle_client_event(
    world: &AdmittedWorldHandle<'_>,
    request: ClientEventRequest,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let event = parse_client_event(request, packet)?;
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;
    match event {
        ClientEvent::NewCareerItemState(report) => {
            tracing::trace!(
                nickname = %identity.nickname,
                career_id = report.career_id,
                state = report.state,
                quantity = report.quantity,
                category = report.category,
                unknown_1 = report.unknown_1,
                unknown_2 = report.unknown_2,
                "accepted bounded P5136 career item-state telemetry"
            );
        }
        ClientEvent::ReportUdpReconnect => {
            world.authorize_udp_rebind().await?;
            tracing::debug!(
                nickname = %identity.nickname,
                generation = identity.generation.get(),
                "authorized same-generation P5136 UDP endpoint rebind"
            );
        }
    }
    Ok(Vec::new())
}

async fn handle_single_player_request(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    kind: SinglePlayerRequestKind,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let request = parse_single_player_request(kind, packet)?;
    match request {
        SinglePlayerRequest::StartSingle(request) => {
            let identity = world.authorize_identity().await?;
            let _ = context.bound_profile_for(&identity)?;
            tracing::trace!(
                nickname = %identity.nickname,
                start_ticks = request.start_ticks,
                proof_length = request.producer_proof.len(),
                "accepted complete P5136 single-player start proof"
            );
            Ok(Vec::new())
        }
        SinglePlayerRequest::UseItem(request) => {
            let identity = world.authorize_identity().await?;
            let _ = context.bound_profile_for(&identity)?;
            tracing::trace!(
                nickname = %identity.nickname,
                item_type = request.item_type,
                operation_type = request.operation_type,
                slot_changer = request.slot_changer,
                "accepted currently non-mutating P5136 single-player item event"
            );
            Ok(Vec::new())
        }
        SinglePlayerRequest::KartSpec(request) => {
            let identity = world.authorize_identity().await?;
            let profile = context.profile_for(&identity)?;
            let physics = selected_physics_metadata(
                profile,
                profiles.catalog(),
                request.kart_id,
                request.flying_pet_id,
                request.speed_type,
                false,
            )?;
            trace_single_player_physics_fallback(&identity, &physics, "PqKartSpec");
            Ok(vec![serialize_kart_spec_reply(&physics.block)])
        }
        SinglePlayerRequest::StartTimeAttack(request) => {
            handle_start_time_attack(world, profiles, request, context).await
        }
        SinglePlayerRequest::FinishTimeAttack(request) => {
            handle_finish_time_attack(world, profiles, request, context).await
        }
    }
}

async fn handle_start_time_attack(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    request: StartTimeAttackRequest,
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    context.ensure_time_attack_idle()?;
    let track = if request.requested_track == 0 {
        P5136_FALLBACK_TRACK_ID
    } else {
        request.requested_track
    };
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, "start time attack")
        .await?;
    let (profile, lane) = profiles
        .start_time_attack(before.nickname.clone(), request, track, admission)
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    let physics = selected_physics_metadata(
        &profile.profile,
        profiles.catalog(),
        request.kart_id,
        request.flying_pet_id,
        request.speed_type,
        request.start_type == 0,
    )?;
    trace_single_player_physics_fallback(&after, &physics, "PqStartTimeAttack");
    let response = serialize_start_time_attack_reply(
        request.start_token,
        &physics.block,
        profile.profile.rider.lucci,
        profile.profile.rider.koin,
        track,
    );
    context.bind_profile(after, profile);
    context.begin_time_attack(request, track);
    drop(lane);
    Ok(vec![response])
}

async fn handle_finish_time_attack(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    request: FinishTimeAttackRequest,
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let active = context.active_time_attack()?;
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, "finish time attack")
        .await?;
    let (applied, profile, lane) = profiles
        .finish_time_attack(before.nickname.clone(), request, admission)
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    debug_assert_eq!(profile.profile.rider.track, active.track);
    debug_assert_eq!(
        applied.earned_rp,
        finish_reward(request.reward_type).earned_rp()
    );
    debug_assert_eq!(
        applied.earned_lucci,
        finish_reward(request.reward_type).earned_lucci()
    );
    let response = serialize_finish_time_attack_reply(
        request.result_type,
        active.request.attack_type,
        request.reward_type,
        1,
    );
    context.bind_profile(after, profile);
    context.complete_time_attack();
    drop(lane);
    Ok(vec![response])
}

fn trace_single_player_physics_fallback(
    identity: &IdentityBinding,
    physics: &RoomPhysicsMetadata,
    request_name: &'static str,
) {
    if physics.physics_fallback() {
        tracing::warn!(
            nickname = %identity.nickname,
            request_name,
            kart_id = physics.kart_id,
            base_resolution = ?physics.base_resolution,
            fallback_reasons = ?physics.fallback_reasons,
            "using bounded single-player kart physics with omitted optional contributions"
        );
    }
}

async fn handle_telemetry_request(
    world: &AdmittedWorldHandle<'_>,
    kind: TelemetryRequestKind,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let report = parse_telemetry_request(kind, packet)?;
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;
    match report {
        TelemetryReport::GameAiReport => {
            tracing::trace!(nickname = %identity.nickname, "accepted bounded GameAi report");
        }
        TelemetryReport::GameReport {
            plane_check,
            distance,
        } => {
            tracing::trace!(
                nickname = %identity.nickname,
                plane_check,
                distance,
                "accepted bounded client anti-cheat report without granting authority"
            );
        }
        TelemetryReport::GameClientFrame {
            local_frame,
            server_frame,
            acknowledged_frame,
        } => {
            tracing::trace!(
                nickname = %identity.nickname,
                local_frame,
                server_frame,
                acknowledged_frame,
                "accepted bounded client-frame telemetry"
            );
        }
        TelemetryReport::GameRequestRelay { value, route_hash } => {
            tracing::trace!(
                nickname = %identity.nickname,
                value,
                route_hash,
                "accepted disabled C# game-relay probe without peer fanout"
            );
        }
        TelemetryReport::RideEventReport { event_count } => {
            tracing::trace!(
                nickname = %identity.nickname,
                event_count,
                "accepted bounded ride-event telemetry"
            );
        }
        TelemetryReport::RidePathReport { sample_count } => {
            tracing::trace!(
                nickname = %identity.nickname,
                sample_count,
                "accepted bounded ride-path telemetry"
            );
        }
        TelemetryReport::UnidentifiedDrivingReport { logical_length } => {
            tracing::trace!(
                nickname = %identity.nickname,
                packet_hash = p5136_core::telemetry_protocol::UNIDENTIFIED_DRIVING_REPORT_HASH,
                logical_length,
                "accepted isolated captured driving report with unidentified semantics"
            );
        }
    }
    Ok(Vec::new())
}

async fn handle_captured_query_request(
    world: &AdmittedWorldHandle<'_>,
    request: CapturedQueryRequest,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let reply = process_captured_query_request(request, packet)?;
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;
    Ok(vec![reply])
}

async fn handle_scenario_request(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    request: ScenarioRequest,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    if request == ScenarioRequest::Complete {
        parse_complete_scenario_request(packet)?;
        let identity = world.authorize_identity().await?;
        let profile = context.profile_for(&identity)?;
        return Ok(vec![serialize_complete_scenario_reply(
            profile.rider.scenario_type,
        )]);
    }

    let start = parse_start_scenario_request(packet)?;
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, "update scenario type")
        .await?;
    let (profile, lane) = profiles
        .update_scenario_type(before.nickname.clone(), start.scenario_type, admission)
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    let _ = context.profile_for(&after)?;
    context.bind_profile(after, profile);
    drop(lane);
    Ok(vec![serialize_start_scenario_reply(start.scenario_type)])
}

async fn dispatch_profile_bound_request(
    services: &SessionServices<'_>,
    world: &AdmittedWorldHandle<'_>,
    hash: u32,
    packet: &[u8],
    context: &mut SessionContext,
) -> Option<Result<Vec<Vec<u8>>, LoginSessionError>> {
    if let Some(request) = classify_equipment_request(hash) {
        return Some(
            dispatch_equipment_request(world, services.profiles, request, packet, context).await,
        );
    }
    if classify_item_state_request(hash).is_some() {
        return Some(
            handle_item_state_request(services.config, world, services.profiles, packet, context)
                .await,
        );
    }
    if let Some(request) = classify_startup_request(hash) {
        return Some(
            handle_startup_request(
                world,
                services.profiles,
                services.session_id,
                request,
                packet,
                context,
            )
            .await,
        );
    }
    if let Some(request) = classify_scenario_request(hash) {
        return Some(
            handle_scenario_request(world, services.profiles, request, packet, context).await,
        );
    }
    if let Some(request) = classify_single_player_request(hash) {
        return Some(
            handle_single_player_request(world, services.profiles, request, packet, context).await,
        );
    }
    if let Some(request) = classify_telemetry_request(hash) {
        return Some(handle_telemetry_request(world, request, packet, context).await);
    }
    None
}

async fn dispatch_fail_closed_request(
    world: &AdmittedWorldHandle<'_>,
    hash: u32,
    packet: &[u8],
    context: &SessionContext,
) -> Option<Result<Vec<Vec<u8>>, LoginSessionError>> {
    if hash == GET_RIDER_INFO_REQUEST_HASH {
        return Some(handle_get_rider_info_failure(world, packet, context).await);
    }
    if classify_club_query_request(hash).is_some() {
        return Some(handle_club_query(world, packet, context).await);
    }
    if classify_shop_buy_request(hash).is_some() {
        return Some(handle_shop_buy_failure(world, packet, context).await);
    }
    None
}

async fn handle_get_rider_info_failure(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Parse the entire stock request before packet-specific authorization so
    // malformed live traffic cannot be mistaken for a valid lookup. Global
    // generation and quiesce admission still precede this handler.
    let _request = parse_get_rider_info_request(packet)?;
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;

    // Cross-profile visibility and offline lookup policy are not yet defined.
    // Fail closed without logging the target, touching profile storage, or
    // publishing a request-specific World command.
    Ok(vec![serialize_get_rider_info_failure()])
}

async fn handle_club_query(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Exact producer-shape validation precedes packet-specific identity and
    // profile checks. Global stale-generation and shutdown admission remain
    // outside this handler.
    let request = parse_club_query_request(packet)?;
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;

    // There is no authoritative club repository yet. Return only the
    // consumer-evidenced empty/unavailable state without request-specific
    // World commands, profile I/O, persistence, mutation, or peer fanout.
    let response = match request.kind() {
        ClubQueryRequest::CheckMyClubState => serialize_no_club_state_reply()?,
        ClubQueryRequest::GetUserWaitingJoinClub => serialize_no_pending_club_join_reply()?,
        ClubQueryRequest::CheckCreateClubCondition => serialize_club_creation_unavailable_reply(),
        ClubQueryRequest::GetClubListCount => serialize_empty_club_list_count_reply(),
        ClubQueryRequest::GetClubWaitingCrewCount => {
            serialize_unavailable_waiting_crew_count_reply()
        }
    };
    Ok(vec![response])
}

enum ItemStateDisposition {
    UnsupportedNoReply,
    FavoriteUpdateApplied,
    LockedUpdateApplied,
    FavoriteSnapshot(Vec<u8>),
    LockedSnapshot(Vec<u8>),
}

impl ItemStateDisposition {
    fn into_packets(self) -> Vec<Vec<u8>> {
        match self {
            Self::UnsupportedNoReply | Self::FavoriteUpdateApplied | Self::LockedUpdateApplied => {
                Vec::new()
            }
            Self::FavoriteSnapshot(packet) | Self::LockedSnapshot(packet) => vec![packet],
        }
    }
}

async fn handle_item_state_request(
    config: &ServerConfig,
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Validate the complete producer shape before packet-specific identity or
    // profile work. The outer dispatch has already retained global shutdown
    // and exact-generation admission.
    let request = parse_item_state_request(packet)?;
    let kind = request.kind();
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;

    let disposition = match kind {
        ItemStateRequest::DeleteItem | ItemStateRequest::UnlockItem => {
            // Both stock reply objects are consumer-side success
            // capabilities, even when the unlock byte is zero. Until an
            // authoritative mutation/authentication domain exists, a
            // well-formed request is explicitly unsupported with no reply.
            tracing::debug!(
                request_kind = kind.request_name(),
                packet_length = packet.len(),
                "leaving unsupported item-state request unanswered"
            );
            ItemStateDisposition::UnsupportedNoReply
        }
        ItemStateRequest::FavoriteItemGet => {
            handle_favorite_item_collection(
                config,
                world,
                profiles,
                &before,
                Vec::new(),
                true,
                context,
            )
            .await?
        }
        ItemStateRequest::FavoriteItemUpdate => {
            let changes = request.into_favorite_changes()?;
            handle_favorite_item_collection(
                config, world, profiles, &before, changes, false, context,
            )
            .await?
        }
        ItemStateRequest::LockedItemGet => {
            handle_locked_item_collection(
                config,
                world,
                profiles,
                &before,
                Vec::new(),
                true,
                context,
            )
            .await?
        }
        ItemStateRequest::LockedItemUpdate => {
            let changes = request.into_locked_changes()?;
            handle_locked_item_collection(config, world, profiles, &before, changes, false, context)
                .await?
        }
    };
    Ok(disposition.into_packets())
}

async fn handle_favorite_item_collection(
    config: &ServerConfig,
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    before: &IdentityBinding,
    changes: Vec<p5136_core::item_state_protocol::FavoriteItemChange>,
    snapshot_requested: bool,
    context: &mut SessionContext,
) -> Result<ItemStateDisposition, LoginSessionError> {
    let operation = if snapshot_requested {
        "load favorite-item snapshot"
    } else {
        FAVORITE_ITEM_UPDATE_OPERATION
    };
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, operation)
        .await?;
    let (receipt, lane) = profiles
        .update_favorite_items(
            before.nickname.clone(),
            changes,
            maximum_item_collection_records(config),
            admission,
        )
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(before, &after)?;
    context.apply_favorite_item_write(&after, &receipt)?;
    let disposition = if snapshot_requested {
        ItemStateDisposition::FavoriteSnapshot(serialize_favorite_item_list(
            receipt.items().as_slice(),
            config.max_login_payload,
        )?)
    } else {
        ItemStateDisposition::FavoriteUpdateApplied
    };
    drop(lane);
    Ok(disposition)
}

async fn handle_locked_item_collection(
    config: &ServerConfig,
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    before: &IdentityBinding,
    changes: Vec<p5136_core::item_state_protocol::FavoriteItemChange>,
    snapshot_requested: bool,
    context: &mut SessionContext,
) -> Result<ItemStateDisposition, LoginSessionError> {
    let operation = if snapshot_requested {
        "load locked-item snapshot"
    } else {
        LOCKED_ITEM_UPDATE_OPERATION
    };
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, operation)
        .await?;
    let (receipt, lane) = profiles
        .update_locked_items(
            before.nickname.clone(),
            changes,
            maximum_item_collection_records(config),
            admission,
        )
        .await?;
    let after = world.authorize_identity().await?;
    ensure_identity_fence(before, &after)?;
    context.apply_locked_item_write(&after, &receipt)?;
    let disposition = if snapshot_requested {
        ItemStateDisposition::LockedSnapshot(serialize_locked_item_list(
            receipt.items().as_slice(),
            config.max_login_payload,
        )?)
    } else {
        ItemStateDisposition::LockedUpdateApplied
    };
    drop(lane);
    Ok(disposition)
}

fn maximum_item_collection_records(config: &ServerConfig) -> usize {
    favorite_item_list_capacity(config.max_login_payload)
        .min(DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS)
}

async fn handle_shop_buy_failure(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let identity = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&identity)?;

    let request = match parse_shop_buy_request(packet) {
        Ok(parsed) => parsed,
        Err(error) => match &error {
            ShopProtocolError::Packet(_) | ShopProtocolError::TrailingBytes { .. } => {
                tracing::debug!(
                    packet_length = packet.len(),
                    %error,
                    "dropping malformed P5136 shop-buy request"
                );
                return Ok(Vec::new());
            }
            ShopProtocolError::UnsupportedPacketHash { .. } => return Err(error.into()),
        },
    };

    // Fail closed without logging request fields or forwarding the purchase
    // into a World domain command. Only bounded, non-sensitive metadata is
    // retained for diagnostics.
    tracing::debug!(
        request_kind = request.kind().request_name(),
        packet_length = packet.len(),
        "rejecting P5136 shop-buy request fail-closed"
    );
    Ok(vec![serialize_shop_buy_failure()])
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

async fn handle_client_endpoint_report(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    kind: ClientEndpointReportKind,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Exact framing and cross-kind hash validation happen before profile
    // admission or actor mutation. The claimed address bytes never leave the
    // codec; only the port is available here.
    let report = parse_client_endpoint_report(kind, packet)?;
    let before = world.authorize_identity().await?;
    let _ = context.profile_for(&before)?;

    if kind == ClientEndpointReportKind::GameUdp {
        tracing::trace!(
            nickname = %before.nickname,
            port = report.port(),
            "validated client game-UDP report without changing actor endpoint authority"
        );
        return Ok(Vec::new());
    }

    let admission = profiles
        .admit_for_operation(
            world.operation(),
            &before.nickname,
            PROFILE_PRESENTATION_WRITE_OPERATION,
        )
        .await?;
    let completion = world
        .reserve_profile_presentation_completion()
        .await
        .map_err(profile_presentation_write_error)?;
    let (prepared, response) = PreparedProfilePresentationWrite::new(
        admission,
        before.clone(),
        ProfilePresentationMutation::SetP2pPort(report.port()),
        completion,
    );
    prepared.submit();
    let receipt = response
        .await
        .map_err(|_| profile_presentation_write_error(ProfilePresentationWriteError::WorldStopped))?
        .map_err(profile_presentation_write_error)?;

    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    if !receipt.publication().updates_runtime_caches() {
        return Err(profile_presentation_write_error(
            ProfilePresentationWriteError::UnexpectedPublication {
                publication: receipt.publication(),
            },
        ));
    }
    context.apply_profile_presentation_write(&after, &receipt)?;
    tracing::trace!(
        nickname = %after.nickname,
        port = report.port(),
        revision = receipt.revision(),
        "installed durable P2P port for the exact session generation"
    );
    Ok(Vec::new())
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
    let presentation = myroom_profile_presentation(&profile.profile).with_p2p_port(0);
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
            RoomCommandPayload::Create {
                request,
                participant: context.room_participant_for(&identity, profiles.catalog())?,
            }
        }
        RoomProtocolRequest::JoinRoom => {
            let request = parse_ch_join_room_request(packet)?;
            let identity = world.authorize_identity().await?;
            RoomCommandPayload::Join {
                request,
                participant: context.room_participant_for(&identity, profiles.catalog())?,
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
    context: Option<&SessionContext>,
    catalog: Option<&CatalogInventory>,
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
        LobbyRequest::ChangeTrack => {
            LobbyCommandPayload::ChangeTrack(parse_change_track_request(packet)?)
        }
        LobbyRequest::BasicAi => LobbyCommandPayload::BasicAi {
            request: parse_basic_ai_request(packet)?,
            candidates: basic_ai_candidates(catalog),
        },
        LobbyRequest::CloseSlot => {
            LobbyCommandPayload::CloseSlot(parse_close_slot_request(packet)?)
        }
        LobbyRequest::RiderTalk => {
            LobbyCommandPayload::RiderTalk(parse_rider_talk_request(packet)?)
        }
        LobbyRequest::MacroChat => {
            let request = parse_macro_chat_request(packet)?;
            let identity = world.authorize_identity().await?;
            let profile = context
                .ok_or(LoginSessionError::ProfileNotBound)?
                .profile_for(&identity)?;
            let messages = if request.chat_type == 0 {
                &profile.game_option.quick_messages
            } else {
                &profile.game_option.team_quick_messages
            };
            let resolved_message = messages
                .get(&i32::from(request.message_id))
                .cloned()
                .unwrap_or_default();
            LobbyCommandPayload::MacroChat {
                request,
                resolved_message,
            }
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

fn basic_ai_candidates(catalog: Option<&CatalogInventory>) -> [RoomAi; 2] {
    fn ids(catalog: Option<&CatalogInventory>, category: u16) -> Vec<i16> {
        catalog
            .into_iter()
            .flat_map(|catalog| catalog.category(category))
            .filter(|item| item.id != 0)
            .take(2)
            .map(|item| i16::from_le_bytes(item.id.to_le_bytes()))
            .collect()
    }

    let characters = ids(catalog, 1);
    let karts = ids(catalog, 3);
    array::from_fn(|index| RoomAi {
        character: characters
            .get(index)
            .or_else(|| characters.first())
            .copied()
            .unwrap_or(1),
        rider: 0,
        kart: karts
            .get(index)
            .or_else(|| karts.first())
            .copied()
            .unwrap_or(1),
        balloon: 0,
        head_band: 0,
        goggle: 0,
        team: 0,
    })
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
        RaceRequest::GameSlot => return handle_game_slot_request_admitted(world, packet).await,
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

async fn handle_game_slot_request_admitted(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let parsed = match parse_game_slot_packet(packet) {
        Ok(parsed) => parsed,
        Err(reason) => {
            tracing::debug!(
                session_id = world.session_id().get(),
                packet_length = packet.len(),
                reason = %reason,
                "dropping a malformed or unsupported TCP GameSlot packet"
            );
            return Ok(Vec::new());
        }
    };
    let player_id = parsed.player_id();
    let packet_type = parsed.body().packet_type();
    let item_or_recipient_mask = parsed.item_or_recipient_mask();

    match world
        .race_command(RaceCommandPayload::GameSlot(parsed))
        .await
    {
        Ok(RaceCommandOutcome::GameSlotEvidencePending {
            room_id,
            race_epoch,
            reason,
            ..
        }) => {
            tracing::debug!(
                session_id = world.session_id().get(),
                room_id = room_id.0,
                race_epoch,
                packet_length = packet.len(),
                player_id,
                packet_type,
                item_or_recipient_mask,
                ?reason,
                "holding a validated TCP GameSlot packet at an explicit evidence boundary"
            );
        }
        Ok(RaceCommandOutcome::GameSlotRelayed {
            room_id,
            race_epoch,
            recipients,
        }) => {
            tracing::trace!(
                session_id = world.session_id().get(),
                room_id = room_id.0,
                race_epoch,
                recipients,
                packet_length = packet.len(),
                player_id,
                packet_type,
                item_or_recipient_mask,
                "relayed a validated TCP GameSlot packet through the World actor"
            );
        }
        Ok(RaceCommandOutcome::GameSlotItemAwarded {
            room_id,
            race_epoch,
            item_id,
            rank_band,
            recipients,
        }) => {
            tracing::debug!(
                session_id = world.session_id().get(),
                room_id = room_id.0,
                race_epoch,
                recipients,
                player_id,
                packet_type,
                item_id,
                ?rank_band,
                "synthesized and broadcast an authoritative item pickup"
            );
        }
        Ok(outcome) => {
            tracing::error!(
                ?outcome,
                session_id = world.session_id().get(),
                player_id,
                packet_type,
                "the World actor returned an invalid outcome for a TCP GameSlot command"
            );
            return Err(LoginSessionError::UnexpectedGameSlotOutcome);
        }
        Err(WorldError::Race(error)) if error.is_expected_rejection() => {
            tracing::debug!(
                %error,
                session_id = world.session_id().get(),
                packet_length = packet.len(),
                player_id,
                packet_type,
                item_or_recipient_mask,
                "dropped a TCP GameSlot packet without terminating the session"
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
        MyRoomRequest::CharacterPosition | MyRoomRequest::RiderTalk => {
            let payload = parse_myroom_peer_payload(request, packet)?
                .expect("peer MyRoom variants always produce a peer command payload");
            execute_myroom_peer_command(world, payload).await?;
            Ok(Vec::new())
        }
        MyRoomRequest::Enter => {
            execute_myroom_entry(
                world,
                SessionMyRoomEntryIntent::Direct(parse_enter_request(packet)?),
                context,
            )
            .await?;
            Ok(Vec::new())
        }
        MyRoomRequest::Reenter => {
            parse_reenter_request(packet)?;
            execute_myroom_entry(world, SessionMyRoomEntryIntent::Reenter, context).await?;
            Ok(Vec::new())
        }
        MyRoomRequest::EnterRandom => {
            parse_enter_random_request(packet)?;
            execute_myroom_entry(world, SessionMyRoomEntryIntent::Random, context).await?;
            Ok(Vec::new())
        }
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
            Ok(Vec::new())
        }
        MyRoomRequest::RequestItems => {
            parse_request_items(packet)?;
            execute_myroom_owner_items(world, profiles, session_id, context).await?;
            Ok(Vec::new())
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
            Ok(Vec::new())
        }
        MyRoomRequest::CheckPassword => handle_myroom_check_password(world, packet, context).await,
        MyRoomRequest::RequestEmblems => {
            execute_myroom_owner_emblems(world, profiles, packet, context).await?;
            Ok(Vec::new())
        }
        MyRoomRequest::RequestCareerList => {
            execute_myroom_owner_career_list(world, packet, context).await?;
            Ok(Vec::new())
        }
        MyRoomRequest::UpdateMainEmblem => {
            update_main_emblems(world, profiles, packet, context).await
        }
        MyRoomRequest::UpdateInfo => update_myroom_info(world, profiles, packet, context).await,
    }
}

async fn update_myroom_info(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
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

async fn execute_myroom_owner_emblems(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    packet: &[u8],
    context: &SessionContext,
) -> Result<(), LoginSessionError> {
    parse_request_emblems(packet)?;
    let Some(plan) = world.prepare_myroom_owner_emblems().await? else {
        return Ok(());
    };
    let _ = context.profile_for(plan.expected_identity())?;
    let emblems = profiles
        .emblem_catalog()
        .map(EmblemCatalog::ids)
        .unwrap_or_default();
    let packet = serialize_owner_emblems(emblems)?;
    match world.publish_myroom_owner_emblems(plan, packet).await {
        Ok(()) => Ok(()),
        Err(WorldError::MyRoomCommandOutboundUnavailable { session }) => {
            tracing::debug!(
                session_id = session.get(),
                "dropping a MyRoom owner-emblem response because its queue is full"
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn execute_myroom_owner_career_list(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
    context: &SessionContext,
) -> Result<(), LoginSessionError> {
    parse_request_career_list(packet)?;
    let Some(plan) = world.prepare_myroom_owner_career_list().await? else {
        return Ok(());
    };
    let _ = context.profile_for(plan.expected_identity())?;
    match world.publish_myroom_owner_career_list(plan).await {
        Ok(()) => Ok(()),
        Err(WorldError::MyRoomCommandOutboundUnavailable { session }) => {
            tracing::debug!(
                session_id = session.get(),
                "dropping a terminal MyRoom owner-Career response because its queue is full"
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
}

async fn update_main_emblems(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    packet: &[u8],
    context: &mut SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    // Exact decoding happens before authorization, catalog lookup, completion
    // reservation, or profile admission, so malformed bodies are side-effect
    // free.
    let request = parse_update_main_emblem(packet)?;
    let Some(plan) = world.prepare_main_emblem_write().await? else {
        return Ok(vec![serialize_update_main_emblem_reply(false)]);
    };
    let before = plan.expected_identity().clone();
    let _ = context.profile_for(&before)?;
    let selection = match profiles.validate_main_emblems(request) {
        Ok(selection) => selection,
        Err(error) => {
            tracing::debug!(
                nickname = %before.nickname,
                %error,
                "rejected a main-emblem selection outside the authoritative catalog"
            );
            return Ok(vec![serialize_update_main_emblem_reply(false)]);
        }
    };
    let admission = profiles
        .admit_for_operation(
            world.operation(),
            &before.nickname,
            MAIN_EMBLEM_WRITE_OPERATION,
        )
        .await?;
    let completion = match world.reserve_main_emblem_completion().await {
        Ok(completion) => completion,
        Err(error) if error.is_request_rejection() => {
            tracing::debug!(
                %error,
                "rejected a main-emblem write before persistence registration"
            );
            return Ok(vec![serialize_update_main_emblem_reply(false)]);
        }
        Err(error) => return Err(main_emblem_write_error(error)),
    };
    let prepared = profiles.prepare_main_emblem_write(selection, admission, completion);
    let receipt = match world.persist_main_emblems(plan, prepared).await {
        Ok(receipt) => receipt,
        Err(error)
            if error.is_request_rejection()
                || matches!(&error, MainEmblemWriteError::Persistence(_)) =>
        {
            tracing::warn!(%error, "main-emblem write did not reach a durable success");
            return Ok(vec![serialize_update_main_emblem_reply(false)]);
        }
        Err(error) => return Err(main_emblem_write_error(error)),
    };
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    if receipt.publication() == MainEmblemPublication::ActiveOwnerCacheUpdated {
        context.apply_main_emblem_write(&after, &receipt)?;
        tracing::trace!(
            nickname = %after.nickname,
            revision = ?receipt.revision(),
            selection = ?receipt.selection().values(),
            "applied durable main emblems to the bound session profile"
        );
    }
    // The actor publishes the pre-reserved success packet only after durable
    // completion and exact owner-generation revalidation.
    Ok(Vec::new())
}

fn parse_myroom_peer_payload(
    request: MyRoomRequest,
    packet: &[u8],
) -> Result<Option<MyRoomPeerCommandPayload>, MyRoomProtocolError> {
    Ok(match request {
        MyRoomRequest::CharacterPosition => Some(MyRoomPeerCommandPayload::CharacterPosition(
            parse_character_position(packet)?,
        )),
        MyRoomRequest::RiderTalk => Some(MyRoomPeerCommandPayload::RiderTalk(parse_rider_talk(
            packet,
        )?)),
        _ => None,
    })
}

async fn handle_myroom_check_password(
    world: &AdmittedWorldHandle<'_>,
    packet: &[u8],
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let request = parse_check_password(packet)?;
    let password_kind = request.password_kind;
    let identity = world.authorize_identity().await?;
    let _ = context.profile_for(&identity)?;
    let status = world.myroom_check_password(request).await?;
    Ok(vec![serialize_check_password_reply(password_kind, status)])
}

enum SessionMyRoomEntryIntent {
    Direct(EnterMyRoomRequest),
    Reenter,
    Random,
}

async fn execute_myroom_entry(
    world: &AdmittedWorldHandle<'_>,
    intent: SessionMyRoomEntryIntent,
    context: &SessionContext,
) -> Result<(), LoginSessionError> {
    let identity = world.authorize_identity().await?;
    let profile = context.profile_for(&identity)?;
    let presentation = context.myroom_presentation_for(&identity)?;
    let input = match intent {
        SessionMyRoomEntryIntent::Direct(request) => {
            let self_info = if canonical_nickname_key(&request.owner_nickname)
                == canonical_nickname_key(&identity.nickname)
            {
                Some(profile.my_room.try_to_protocol_info()?)
            } else {
                None
            };
            MyRoomEntryInput::direct(
                identity,
                request.owner_nickname,
                request.password,
                &presentation,
                self_info,
            )
        }
        SessionMyRoomEntryIntent::Reenter => MyRoomEntryInput::reenter(
            identity,
            &presentation,
            profile.my_room.try_to_protocol_info(),
        ),
        SessionMyRoomEntryIntent::Random => MyRoomEntryInput::random(identity, &presentation),
    }
    .map_err(|source| LoginSessionError::MyRoomEntryPreparation {
        source: Box::new(source),
    })?;
    world.myroom_enter(input).await?;
    Ok(())
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

async fn execute_myroom_peer_command(
    world: &AdmittedWorldHandle<'_>,
    payload: MyRoomPeerCommandPayload,
) -> Result<(), LoginSessionError> {
    match world.myroom_peer_command(payload).await {
        Ok(()) => Ok(()),
        Err(WorldError::MyRoomCommandOutboundUnavailable { session }) => {
            tracing::debug!(
                session_id = world.session_id().get(),
                unavailable_session_id = session.get(),
                "dropping an atomic MyRoom peer fanout because a recipient queue is full"
            );
            Ok(())
        }
        Err(error) => Err(error.into()),
    }
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

fn main_emblem_write_error(source: MainEmblemWriteError) -> LoginSessionError {
    LoginSessionError::MainEmblemWrite {
        source: Box::new(source),
    }
}

fn profile_presentation_write_error(source: ProfilePresentationWriteError) -> LoginSessionError {
    LoginSessionError::ProfilePresentationWrite {
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
        EquipmentRequest::EquipXPart => {
            let request = parse_equip_x_part(packet)?;
            equip_x_part(world, profiles, request, context).await
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

async fn equip_x_part(
    world: &AdmittedWorldHandle<'_>,
    profiles: &ProfileCoordinator,
    request: XPartEquipRequest,
    context: &SessionContext,
) -> Result<Vec<Vec<u8>>, LoginSessionError> {
    let before = world.authorize_identity().await?;
    let _ = context.bound_profile_for(&before)?;
    let admission = profiles
        .admit_for_operation(world.operation(), &before.nickname, "equip X-part")
        .await?;
    let (equipped, lane) = match profiles.equip_x_part(request, admission).await {
        Ok(result) => result,
        Err(error) => {
            tracing::warn!(%error, "failed to persist P5136 X-part selection");
            return Ok(vec![serialize_equip_x_part_failure(request)]);
        }
    };
    let after = world.authorize_identity().await?;
    ensure_identity_fence(&before, &after)?;
    let _ = context.profile_for(&after)?;
    drop(lane);
    if !equipped {
        tracing::warn!(
            category = request.item_category,
            item_id = request.item_id,
            kart_id = request.kart_id,
            kart_serial = request.kart_serial,
            "rejected ungranted P5136 X-part without terminating the session"
        );
        return Ok(vec![serialize_equip_x_part_failure(request)]);
    }
    Ok(vec![serialize_equip_x_part_success(request)])
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
    selected_physics_metadata(
        profile,
        catalog,
        profile.rider_item.kart,
        profile.rider_item.flying_pet,
        profile.rider.speed_type,
        true,
    )
}

fn selected_physics_metadata(
    profile: &Profile,
    catalog: Option<&CatalogInventory>,
    kart_id: u16,
    flying_pet_id: u16,
    requested_speed_type: u8,
    apply_profile_equipment: bool,
) -> Result<RoomPhysicsMetadata, KartPhysicsBuildError> {
    let items = &profile.rider_item;
    let mut snapshot = P5136KartPhysicsSnapshot::csharp_s7_baseline();
    // The catalog currently carries kart bodies, while the exported runtime
    // settings select the S7 speed preset. The request byte still controls the
    // two S4/S6 sentinel branches in the reference serializer.
    snapshot.speed_type = if matches!(requested_speed_type, 4 | 6) {
        requested_speed_type
    } else {
        7
    };
    let mut fallback_reasons = Vec::new();
    let base_resolution = if kart_id == 0 {
        RoomKartBaseResolution::KartZeroBaseline
    } else if let Some(catalog) = catalog {
        if let Some(spec) = catalog.kart_spec(kart_id) {
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

    if flying_pet_id != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::FlyingPetNotApplied {
            item_id: flying_pet_id,
        });
    }
    if apply_profile_equipment && kart_id == items.kart {
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
    }
    if profile.server_setting.speed_patch_use != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::SpeedPatchNotApplied {
            value: profile.server_setting.speed_patch_use,
        });
    }
    if kart_id != 0 {
        fallback_reasons.push(RoomPhysicsFallbackReason::TuneLevelV2SidecarsUninspected);
    }

    Ok(RoomPhysicsMetadata {
        kart_id,
        base_resolution,
        fallback_reasons,
        block: build_p5136_kart_physics_block(&snapshot)?,
    })
}

fn room_participant_from_profile_with_p2p_port(
    identity: &IdentityBinding,
    profile: &Profile,
    reported_p2p_port: u16,
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
    let endpoint = legacy_p2p_endpoint(identity.source_ip, reported_p2p_port);
    let club_name = if profile.rider.club_mark_logo == 0 {
        String::new()
    } else {
        profile.rider.club_name.clone()
    };
    Ok(RoomParticipant {
        player: RoomPlayer {
            player_type: if observer { 4 } else { 2 },
            user_no: identity.user_no.get(),
            p2p_address: endpoint.address(),
            p2p_port: endpoint.port(),
            nickname: identity.nickname.clone(),
            emblem_1: u16::from_le_bytes(profile.rider.emblem1.to_le_bytes()),
            emblem_2: u16::from_le_bytes(profile.rider.emblem2.to_le_bytes()),
            emblem_3: u16::from_le_bytes(profile.rider.emblem3.to_le_bytes()),
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

#[cfg(test)]
fn room_participant_from_profile(
    identity: &IdentityBinding,
    profile: &Profile,
    catalog: Option<&CatalogInventory>,
) -> Result<RoomParticipant, LoginSessionError> {
    room_participant_from_profile_with_p2p_port(
        identity,
        profile,
        u16::try_from(profile.rider.p2p_port).unwrap_or_default(),
        catalog,
    )
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
    match request {
        StartupRequest::GetRiderTaskContext => parse_pq_get_rider_task_context(packet)?,
        StartupRequest::VersusModeRankOne => parse_pq_versus_mode_rank_one(packet)?,
        StartupRequest::RiderSchoolExpiredCheck => {
            parse_pq_rider_school_expired_check(packet)?;
        }
        StartupRequest::RankerInfo => parse_pq_ranker_info(packet)?,
        StartupRequest::GetMaxGiftId => parse_sp_rq_get_max_gift_id(packet)?,
        StartupRequest::KoinBalance => parse_sp_rq_koin_balance(packet)?,
        StartupRequest::FavoriteTrackMap => parse_pq_favorite_track_map_get(packet)?,
        StartupRequest::GetCashInventory => parse_sp_rq_get_cash_inventory(packet)?,
        StartupRequest::RemainCash => parse_sp_rq_remain_cash(packet)?,
        StartupRequest::RemainTcCash => parse_sp_rq_remain_tc_cash(packet)?,
        StartupRequest::LockedItemList => parse_pq_locked_item_get(packet)?,
        StartupRequest::RequestExtradata => parse_pq_request_extradata(packet)?,
        StartupRequest::WebEventCompleteCheck => parse_pq_web_event_complete_check(packet)?,
        StartupRequest::StartRiderSchool => {
            let _ = parse_pq_start_rider_school(packet)?;
        }
        _ => {}
    }

    let identity = world.authorize_identity().await?;
    let profile = context.profile_for(&identity)?;
    Ok(startup_response(request, profile)?.into_iter().collect())
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
    let presentation = myroom_profile_presentation(&profile.profile)
        .with_p2p_port(context.reported_p2p_port_for(&after)?);
    world
        .refresh_myroom_presentation(after.clone(), MyRoomProfileLease::new(presentation, lane))
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
        emblem_3: u16::from_le_bytes(profile.rider.emblem3.to_le_bytes()),
        rider_item_snapshot: rider_item_snapshot(&profile.rider_item),
        lucci: profile.rider.lucci,
        rp: i32::from_le_bytes(profile.rider.rp.to_le_bytes()),
    }
}

fn startup_response(
    request: StartupRequest,
    profile: &Profile,
) -> Result<Option<Vec<u8>>, KartPhysicsBuildError> {
    let time = current_legacy_time();
    let response = match request {
        StartupRequest::LoginVipInfo => startup::serialize_pr_login_vip_info(profile.rider.premium),
        StartupRequest::EventReward => startup::serialize_lo_rp_event_reward(),
        StartupRequest::AddRacingTime => startup::serialize_lo_rp_add_racing_time(),
        StartupRequest::EquipTuning => startup::serialize_pr_equip_tuning_failure(),
        StartupRequest::VersusModeRankOne => startup::serialize_pr_versus_mode_rank_one(),
        StartupRequest::GetGameOption => {
            startup::serialize_pr_get_game_option(&profile_game_options(&profile.game_option))
        }
        StartupRequest::SetPlaytimeEventTick => startup::serialize_pr_set_playtime_event_tick(),
        StartupRequest::ChapterInfo => startup::serialize_pr_chapter_info(),
        StartupRequest::GetDuelMissionBulk => startup::serialize_pr_get_duel_mission_bulk(time),
        StartupRequest::RiderSchoolData => startup::serialize_pr_rider_school_data(time),
        StartupRequest::RiderSchoolProgress => startup::serialize_pr_rider_school_progress(),
        StartupRequest::RiderSchoolExpiredCheck => {
            startup::serialize_pr_rider_school_expired_check()
        }
        StartupRequest::RankerInfo => startup::serialize_pr_ranker_info(profile.rider.ranker),
        StartupRequest::GetMaxGiftId => startup::serialize_sp_rp_get_max_gift_id(),
        StartupRequest::KoinBalance => startup::serialize_sp_rp_koin_balance(profile.rider.koin),
        StartupRequest::FavoriteTrackMap => startup::serialize_empty_pr_favorite_track_map_get(),
        StartupRequest::GetCashInventory => startup::serialize_empty_sp_rp_get_cash_inventory(),
        StartupRequest::RemainCash => startup::serialize_sp_rp_remain_cash(profile.rider.cash),
        StartupRequest::RemainTcCash => {
            startup::serialize_sp_rp_remain_tc_cash(profile.rider.tc_cash)
        }
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
        StartupRequest::ServerTime => startup::serialize_pr_server_time(time),
        StartupRequest::GetRiderTaskContext => startup::serialize_pr_get_rider_task_context(),
        StartupRequest::LockedItemList => startup::serialize_empty_locked_item_list(),
        StartupRequest::RequestExtradata => startup::serialize_pr_request_extradata(),
        StartupRequest::WebEventCompleteCheck => startup::serialize_pr_web_event_complete_check(),
        StartupRequest::StartRiderSchool => startup::serialize_pr_start_rider_school()?,
        StartupRequest::GetRider | StartupRequest::UpdateGameOption => return Ok(None),
    };
    Ok(Some(response))
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
    handle_lobby_request_admitted(&world.admitted(&operation), request, packet, None, None).await
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
        collections::{BTreeMap, BTreeSet},
        fmt::Write as _,
        fs,
        future::Future,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        path::Path,
        pin::Pin,
        sync::Arc,
        task::{Context, Poll},
        time::{Duration, Instant},
    };

    use p5136_core::{
        adler32,
        captured_query_protocol::{CAPTURED_QUERY_REQUESTS, CapturedQueryRequest},
        channel::serialize_pr_channel_move_in,
        client_event_protocol::ClientEventProtocolError,
        club_query_protocol::{
            ClubQueryProtocolError, ClubQueryRequest, serialize_club_creation_unavailable_reply,
            serialize_empty_club_list_count_reply, serialize_no_club_state_reply,
            serialize_no_pending_club_join_reply, serialize_unavailable_waiting_crew_count_reply,
        },
        equipment_protocol::{
            EquipmentRequest, PlantPartEquipRequest, RiderItemSelection, XPartEquipRequest,
            serialize_equip_tuning_failure, serialize_equip_tuning_success,
            serialize_equip_x_part_failure, serialize_equip_x_part_success,
        },
        frame::{DEFAULT_MAX_PAYLOAD, encode_encrypted},
        game_slot_protocol::{
            GAME_KART_ITEM_INFO_HASH, GAME_SLOT_PACKET_HASH, GAME_SLOT_PACKET_NAME,
            GO_ITEM_CUBE_HASH, GOP_CUBE_HASH, GameSlotBody, parse_game_slot_packet,
        },
        item_state_protocol::{
            DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS, DELETE_ITEM_REQUEST_NAME,
            FAVORITE_ITEM_GET_REQUEST_NAME, FAVORITE_ITEM_UPDATE_REQUEST_NAME, FavoriteItemChange,
            FavoriteItemKey, FavoriteItemOperation, ItemStateProtocolError,
            LOCKED_ITEM_UPDATE_REQUEST_NAME, UNLOCK_ITEM_REQUEST_NAME,
            serialize_favorite_item_list, serialize_locked_item_list,
        },
        kart_physics::{P5136KartPhysicsSnapshot, build_p5136_kart_physics_block},
        lobby_protocol::{LobbyProtocolError, LobbyRequest, PlayerSlotState},
        myroom_protocol::{
            CHAR_POSITION_NAME, CHECK_PASSWORD_REQUEST_NAME, CheckPasswordStatus,
            ENTER_MYROOM_REQUEST_NAME, ENTER_RANDOM_MYROOM_REQUEST_NAME, EnterMyRoomStatus,
            FIRST_MYROOM_REQUEST_NAME, MAX_MYROOM_PASSWORD_UTF16_UNITS,
            MAX_MYROOM_TALK_UTF16_UNITS, MYROOM_SLOT_COUNT, MyRoomInfo, MyRoomKart, MyRoomParts,
            MyRoomProtocolError, MyRoomSlot, MyRoomTune, OWNER_ITEM_NAME,
            REENTER_MYROOM_REQUEST_NAME, REQUEST_CAREER_LIST_NAME, REQUEST_MYROOM_ITEMS_NAME,
            RIDER_TALK_NAME, plan_owner_item_packets, serialize_character_position,
            serialize_check_password_reply, serialize_empty_owner_career_list,
            serialize_enter_error, serialize_enter_reply, serialize_missing_owner_items,
            serialize_myroom_info, serialize_owner_emblems, serialize_owner_item_enchants,
            serialize_owner_items, serialize_password_enter_myroom_command, serialize_rider_echo,
            serialize_secede_reply, serialize_slot_data, serialize_update_main_emblem_reply,
        },
        packet::{PacketError, PacketReader, PacketWriter},
        race_protocol::{
            GameControlRequest, RaceProtocolError, RaceRequest, parse_game_control_request,
        },
        rider_info_protocol::{
            GET_RIDER_INFO_REQUEST_NAME, RiderInfoProtocolError, serialize_get_rider_info_failure,
        },
        room_protocol::{
            ChCreateRoomRequest, ChJoinRoomRequest, MAX_CLUB_NAME_UTF16_UNITS,
            ROOM_CONNECTION_CONTEXT_LENGTH, ROOM_DATA_LENGTH, RoomProtocolError,
            RoomProtocolRequest,
        },
        scenario_protocol::{
            COMPLETE_SCENARIO_REQUEST_NAME, START_SCENARIO_REQUEST_NAME,
            serialize_complete_scenario_reply, serialize_start_scenario_reply,
        },
        shop_protocol::{ShopBuyRequest, serialize_shop_buy_failure},
        startup::{
            GameOptions, LOCKED_ITEM_LIST_REQUEST_NAME, REQUEST_EXTRADATA_REQUEST_NAME,
            RIDER_ITEM_SNAPSHOT_WIRE_LENGTH, START_RIDER_SCHOOL_REQUEST_NAME, StartupError,
            WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME, serialize_empty_locked_item_list,
            serialize_pr_request_extradata, serialize_pr_start_rider_school,
            serialize_pr_web_event_complete_check,
        },
        udp_protocol::parse_routed_udp_packet,
    };
    use p5136_profile::{
        CatalogInventory, EquipmentExceptions, FavoriteItemStateError, FavoriteItems, GrantedKart,
        MyRoomItemStateError, Profile, ProfileStore, favorite_item_snapshot, rider_item_snapshot,
    };
    use tokio::io::{AsyncWrite, AsyncWriteExt, duplex};
    use tokio::sync::{mpsc, oneshot};
    use tokio::time;

    use super::{
        BlockingUpdateHook, FAVORITE_ITEM_UPDATE_OPERATION, LoginSessionError,
        MAX_MYROOM_ITEM_RECORDS, MAX_MYROOM_OWNER_ITEM_BYTES, MAX_MYROOM_OWNER_ITEM_PACKETS,
        MAX_OUTBOUND_BATCH_BURST, MyRoomOwnerItemPacketBatch, ProfileCoordinator,
        RiderEquipmentValidationError, RoomKartBaseResolution, RoomPhysicsFallbackReason,
        SessionContext, SessionReadEvent, SessionServices, dispatch_packet, handle_channel_move_in,
        handle_equipment_request, handle_get_rider, handle_lobby_request, handle_race_request,
        handle_room_request, myroom_player_slot_from_profile, myroom_profile_presentation,
        read_encrypted_frame, read_encrypted_frame_with_diagnostics, read_session_frame,
        room_participant_from_profile, room_physics_metadata, select_session_read_event,
        update_game_options, write_outbound_batch, write_session_bytes,
    };
    use crate::equipment_persistence::validate_rider_item_selection;
    use crate::operation_gate::WireOperationGate;
    use crate::profile_io::MyRoomProfileLease;
    use crate::{
        ChannelBinding, FavoriteItemPersistError, IdentityBinding, IdentityError, MigrationToken,
        ServerConfig, SessionId, WorldError, WorldHandle,
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
                Box::new((1..=1_199).chain([1_450, 1_453]))
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
        assert_eq!(item_count, 6_801);
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
                <Emblems total="3">
                    <Emblem id="7" />
                    <Emblem id="8" />
                    <Emblem id="9" />
                </Emblems>
            </KartCatalog>"#
        );
        Arc::new(CatalogInventory::from_xml(xml.as_bytes()).unwrap())
    }

    fn exact_shop_buy_request(kind: ShopBuyRequest) -> Vec<u8> {
        let mut packet = PacketWriter::named(kind.request_name());
        packet.write_i32(-12_345);
        packet.write_i32(i32::MIN);
        packet.write_u8(0xFF);
        if kind == ShopBuyRequest::ItemPreset {
            packet.write_u16(0xBEEF);
        }
        packet.into_inner()
    }

    fn exact_get_rider_info_request(target_nickname: &str, mode: u8) -> Vec<u8> {
        let mut packet = PacketWriter::named(GET_RIDER_INFO_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_i32(0);
        packet
            .write_utf16(target_nickname)
            .expect("test nickname fits the P5136 length prefix");
        packet.write_u8(mode);
        packet.into_inner()
    }

    fn exact_start_rider_school_request(value: u8) -> Vec<u8> {
        let mut packet = PacketWriter::named(START_RIDER_SCHOOL_REQUEST_NAME);
        packet.write_encoded_u8(value);
        packet.into_inner()
    }

    fn exact_club_query_request(kind: ClubQueryRequest) -> Vec<u8> {
        let mut packet = PacketWriter::named(kind.request_name());
        match kind {
            ClubQueryRequest::CheckMyClubState
            | ClubQueryRequest::GetUserWaitingJoinClub
            | ClubQueryRequest::CheckCreateClubCondition => {}
            ClubQueryRequest::GetClubListCount => {
                packet
                    .write_utf16("ClubFilter")
                    .expect("test club filter fits");
                packet
                    .write_utf16("MasterFilter")
                    .expect("test master filter fits");
            }
            ClubQueryRequest::GetClubWaitingCrewCount => packet.write_u32(10_000),
        }
        packet.into_inner()
    }

    fn expected_club_query_reply(kind: ClubQueryRequest) -> Vec<u8> {
        match kind {
            ClubQueryRequest::CheckMyClubState => {
                serialize_no_club_state_reply().expect("fixed strings fit")
            }
            ClubQueryRequest::GetUserWaitingJoinClub => {
                serialize_no_pending_club_join_reply().expect("fixed string fits")
            }
            ClubQueryRequest::CheckCreateClubCondition => {
                serialize_club_creation_unavailable_reply()
            }
            ClubQueryRequest::GetClubListCount => serialize_empty_club_list_count_reply(),
            ClubQueryRequest::GetClubWaitingCrewCount => {
                serialize_unavailable_waiting_crew_count_reply()
            }
        }
    }

    fn exact_delete_item_request(item: FavoriteItemKey, quantity_or_mode: u16) -> Vec<u8> {
        let mut packet = PacketWriter::named(DELETE_ITEM_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_u32(0);
        packet.write_u16(item.category());
        packet.write_u16(item.item_id());
        packet.write_u16(item.serial());
        packet.write_u16(quantity_or_mode);
        packet.into_inner()
    }

    fn exact_unlock_item_request() -> Vec<u8> {
        let mut packet = PacketWriter::named(UNLOCK_ITEM_REQUEST_NAME);
        packet.write_u32(0);
        packet.write_u32(0);
        packet.write_u8(0);
        packet.into_inner()
    }

    fn exact_favorite_item_update(records: &[(FavoriteItemKey, u8)]) -> Vec<u8> {
        let mut packet = PacketWriter::named(FAVORITE_ITEM_UPDATE_REQUEST_NAME);
        packet.write_u8(1);
        packet.write_u32(u32::try_from(records.len()).expect("test count fits"));
        for (item, operation) in records {
            packet.write_u16(item.category());
            packet.write_u16(item.item_id());
            packet.write_u16(item.serial());
            packet.write_u8(*operation);
        }
        packet.into_inner()
    }

    fn exact_favorite_item_get() -> Vec<u8> {
        PacketWriter::named(FAVORITE_ITEM_GET_REQUEST_NAME).into_inner()
    }

    fn captured_kart_spec_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("PqKartSpec");
        packet.write_u8(0);
        packet.write_u16(1_340);
        packet.write_u16(32);
        packet.write_bytes(&[0x14, 0, 0, 0, 1, 0]);
        packet.into_inner()
    }

    fn time_attack_start_request(track: u32, mode_type: u8) -> Vec<u8> {
        let mut packet = PacketWriter::named("PqStartTimeAttack");
        packet.write_i32(0x2B04_A2B0);
        packet.write_i32(0);
        packet.write_u32(track);
        packet.write_u8(7);
        packet.write_u8(0);
        packet.write_u16(1_401);
        packet.write_u16(32);
        packet.write_u8(0);
        packet.write_i32(0);
        packet.write_i32(0);
        packet.write_u8(0);
        packet.write_u8(0);
        packet.write_u8(mode_type);
        packet.write_i32(0);
        packet.write_u8(0);
        packet.into_inner()
    }

    fn captured_single_start_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("LoRqStartSinglePacket");
        packet.write_i32(0x119E_CCA6);
        packet.write_bytes(&[0x5a; 33]);
        packet.into_inner()
    }

    fn captured_use_item_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("LoRqUseItemPacket");
        packet.write_i16(7);
        packet.write_i16(1);
        packet.write_u16(u16::MAX);
        packet.into_inner()
    }

    fn captured_time_attack_finish_request() -> Vec<u8> {
        let mut packet = PacketWriter::named("PqFinishTimeAttack");
        packet.write_i32(2);
        packet.write_i32(0x0124_0061);
        packet.write_u8(1);
        packet.write_i32(0x119C_4552);
        packet.write_i32(0);
        packet.write_i32(31);
        packet.write_i32(4);
        packet.write_u32(101_731);
        packet.into_inner()
    }

    fn trace_header_value<'a>(line: &'a str, prefix: &str) -> Option<&'a str> {
        line.split('|')
            .map(str::trim)
            .find_map(|field| field.strip_prefix(prefix))
    }

    fn read_retained_rx_packets(directory: &Path) -> Vec<(String, u32, Vec<u8>)> {
        let mut paths = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("packet-trace_"))
                    && path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
            })
            .collect::<Vec<_>>();
        paths.sort();

        let mut packets = Vec::new();
        for path in paths {
            let contents = fs::read_to_string(&path).unwrap();
            let lines = contents.lines().collect::<Vec<_>>();
            for (index, line) in lines.iter().enumerate() {
                if !line.contains("| PACKET |") || trace_header_value(line, "dir=") != Some("RX") {
                    continue;
                }
                let transport = trace_header_value(line, "transport=")
                    .expect("packet trace has a transport")
                    .to_owned();
                let declared_length = trace_header_value(line, "len=")
                    .expect("packet trace has a length")
                    .parse::<usize>()
                    .expect("packet length is numeric");
                let hash = u32::from_str_radix(
                    trace_header_value(line, "hash=")
                        .expect("packet trace has a hash")
                        .trim_start_matches("0x"),
                    16,
                )
                .expect("packet hash is hexadecimal");
                let hex = lines
                    .get(index + 1)
                    .and_then(|line| line.split_once('|').map(|(_, hex)| hex))
                    .expect("packet record is followed by a HEX line");
                let bytes = hex
                    .split_ascii_whitespace()
                    .map(|byte| u8::from_str_radix(byte, 16).expect("HEX byte is hexadecimal"))
                    .collect::<Vec<_>>();
                assert_eq!(bytes.len(), declared_length, "{}", path.display());
                let hash_offset = if transport == "TCP" { 0 } else { 8 };
                assert_eq!(
                    u32::from_le_bytes(
                        bytes[hash_offset..hash_offset + 4]
                            .try_into()
                            .expect("packet has a hash"),
                    ),
                    hash,
                    "{}",
                    path.display()
                );
                packets.push((transport, hash, bytes));
            }
        }
        packets
    }

    fn dispatcher_owns_hash(hash: u32) -> bool {
        hash == adler32::packet_hash("PqCnAuthenLogin")
            || hash == adler32::packet_hash("PqLogin")
            || hash == adler32::packet_hash("PqChannelMovein")
            || hash == adler32::packet_hash("PqChannelSwitch")
            || hash == super::GET_RIDER_INFO_REQUEST_HASH
            || super::classify_club_query_request(hash).is_some()
            || super::classify_shop_buy_request(hash).is_some()
            || super::classify_client_endpoint_report(hash).is_some()
            || super::classify_client_event(hash).is_some()
            || super::classify_room_protocol_request(hash).is_some()
            || super::classify_lobby_request(hash).is_some()
            || super::classify_race_request(hash).is_some()
            || super::classify_myroom_request(hash).is_some()
            || super::classify_equipment_request(hash).is_some()
            || super::classify_item_state_request(hash).is_some()
            || super::classify_startup_request(hash).is_some()
            || super::classify_scenario_request(hash).is_some()
            || super::classify_single_player_request(hash).is_some()
            || super::classify_telemetry_request(hash).is_some()
            || super::classify_captured_query_request(hash).is_some()
            || super::is_startup_noop(hash)
    }

    fn validate_retained_gap_packet(hash: u32, packet: &[u8]) -> bool {
        if let Some(request) = super::classify_captured_query_request(hash) {
            super::process_captured_query_request(request, packet)
                .expect("captured query packet must parse");
            return true;
        }
        if let Some(request) = super::classify_client_event(hash) {
            super::parse_client_event(request, packet).expect("captured client event must parse");
            return true;
        }
        if let Some(request) = super::classify_single_player_request(hash) {
            super::parse_single_player_request(request, packet)
                .expect("captured single-player packet must parse");
            return true;
        }
        if let Some(request) = super::classify_telemetry_request(hash) {
            super::parse_telemetry_request(request, packet)
                .expect("captured telemetry packet must parse");
            return true;
        }
        if let Some(request) = super::classify_scenario_request(hash) {
            match request {
                super::ScenarioRequest::Start => {
                    super::parse_start_scenario_request(packet)
                        .expect("captured scenario-start packet must parse");
                }
                super::ScenarioRequest::Complete => {
                    super::parse_complete_scenario_request(packet)
                        .expect("captured scenario-complete packet must parse");
                }
            }
            return true;
        }
        if super::classify_item_state_request(hash)
            == Some(p5136_core::item_state_protocol::ItemStateRequest::LockedItemUpdate)
        {
            super::parse_item_state_request(packet)
                .expect("captured locked-item update must parse");
            return true;
        }
        if super::classify_equipment_request(hash) == Some(EquipmentRequest::EquipXPart) {
            super::parse_equip_x_part(packet).expect("captured X-parts request must parse");
            return true;
        }
        match super::classify_lobby_request(hash) {
            Some(LobbyRequest::ChangeTrack) => {
                super::parse_change_track_request(packet)
                    .expect("captured room-track request must parse");
            }
            Some(LobbyRequest::BasicAi) => {
                super::parse_basic_ai_request(packet)
                    .expect("captured basic-AI request must parse");
            }
            Some(LobbyRequest::CloseSlot) => {
                super::parse_close_slot_request(packet)
                    .expect("captured close-slot request must parse");
            }
            Some(LobbyRequest::RiderTalk) => {
                super::parse_rider_talk_request(packet)
                    .expect("captured room-talk request must parse");
            }
            Some(LobbyRequest::MacroChat) => {
                super::parse_macro_chat_request(packet)
                    .expect("captured macro-chat request must parse");
            }
            _ => return false,
        }
        true
    }

    fn seed_canonical_favorite_profile(profile_root: &std::path::Path, nickname: &str) {
        let store = ProfileStore::new(profile_root);
        store.load_or_create(nickname).unwrap();
        store
            .update(nickname, |profile| {
                profile.favorite_items = Some(FavoriteItems::default());
            })
            .unwrap();
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

    async fn shutdown_spawned_myroom_test(
        world: &WorldHandle,
        profile_runtime: crate::profile_io::ProfileIoRuntime,
        actor: tokio::task::JoinHandle<Result<(), crate::world::WorldActorError>>,
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

    fn unsupported_game_slot_packet(packet_type: u8) -> Vec<u8> {
        let mut packet = PacketWriter::named(GAME_SLOT_PACKET_NAME);
        packet.write_i32(0);
        packet.write_u32(1);
        packet.write_u8(packet_type);
        packet.into_inner()
    }

    fn item_vector_game_slot_packet() -> Vec<u8> {
        let mut packet = PacketWriter::named(GAME_SLOT_PACKET_NAME);
        packet.write_i32(0);
        packet.write_u32(1);
        packet.write_u8(9);
        packet.write_bytes(&[0; 3]);
        packet.write_u32(8);
        packet.write_u32(GAME_KART_ITEM_INFO_HASH);
        packet.write_u32(0);
        packet.into_inner()
    }

    fn item_pickup_game_slot_packet(packet_type: u8) -> Vec<u8> {
        assert!(matches!(packet_type, 1 | 2));
        let mut packet = PacketWriter::named(GAME_SLOT_PACKET_NAME);
        packet.write_i32(0);
        packet.write_u32(u32::MAX);
        packet.write_u8(packet_type);
        let (object_id, first_tick, second_tick, operation_tick) = if packet_type == 1 {
            (0xf000_0001_u32, 1_000_u32, 2_500_u32, 1_000_u32)
        } else {
            (0x00ff_ffff_u32, 1_000_u32, 1_000_u32, 2_000_u32)
        };
        packet.write_u8(0);
        packet.write_u32(object_id);
        packet.write_u32(first_tick);
        packet.write_u32(second_tick);
        packet.write_bytes(&[0; 12]);
        packet.write_i16(0);
        if packet_type == 1 {
            packet.write_u8(0);
            packet.write_u32(0x0000_ffff);
        } else {
            packet.write_u8(8);
            packet.write_u32(10);
        }
        packet.write_u32(24);
        packet.write_u32(GOP_CUBE_HASH);
        packet.write_u32(GO_ITEM_CUBE_HASH);
        packet.write_u32(object_id);
        packet.write_u32(1);
        packet.write_u32(0);
        packet.write_u32(operation_tick);
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

    fn rider_talk_packet(message: &str) -> Vec<u8> {
        let mut packet = PacketWriter::named(RIDER_TALK_NAME);
        packet.write_utf16(message).unwrap();
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
    async fn diagnostic_reader_keeps_partial_login_frame_failure_typed() {
        let (mut writer, mut reader) = duplex(16);
        writer.write_all(&[1, 2]).await.unwrap();
        writer.shutdown().await.unwrap();
        let mut receive_iv = 0xa1b7_1c9b;

        let error = read_encrypted_frame_with_diagnostics(
            &mut reader,
            &mut receive_iv,
            DEFAULT_MAX_PAYLOAD,
            None,
        )
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            LoginSessionError::Io(ref source) if source.kind() == std::io::ErrorKind::UnexpectedEof
        ));
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
                None,
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
                None,
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
                None,
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
            write_outbound_batch(&mut writer, batch, &mut send_iv, &config, None).await
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
        write_outbound_batch(&mut writer, batch, &mut send_iv, &config, None)
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
        let truncated_track = PacketWriter::named("GrChangeTrackPacket");
        let truncated_ai = PacketWriter::named("GrRequestBasicAiPacket");
        let mut invalid_close = PacketWriter::named("GrRequestClosePacket");
        invalid_close.write_u32(0);
        invalid_close.write_u8(2);
        invalid_close.write_bytes(&[0; 12]);
        let mut oversized_talk = PacketWriter::named("GrRiderTalkPacket");
        oversized_talk.write_i32(257);
        let mut oversized_macro = PacketWriter::named("PqSendMacroChat");
        oversized_macro.write_i32(0);
        oversized_macro.write_u8(0);
        oversized_macro.write_i32(257);

        for packet in [
            invalid_state.into_inner(),
            invalid_team.into_inner(),
            invalid_master.into_inner(),
            trailing_start.into_inner(),
            truncated_track.into_inner(),
            truncated_ai.into_inner(),
            invalid_close.into_inner(),
            oversized_talk.into_inner(),
            oversized_macro.into_inner(),
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
    async fn authenticated_dispatch_replies_to_startup_queries_and_rejects_unknown_packets() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_706))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "UnknownPacketClassifier")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        store.load_or_create(&identity.nickname).unwrap();
        store
            .update(&identity.nickname, |profile| profile.rider.ranker = 7)
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        let hash = 0xDEAD_BEEF_u32;

        for (request_name, expected) in [
            (
                "PqGetRiderTaskContext",
                vec![0x50, 0x08, 0x84, 0x58, 0, 0, 0, 0],
            ),
            (
                "PqVersusModeRankOnePacket",
                vec![
                    0xD5, 0x09, 0xDA, 0x7F, 0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
                ],
            ),
            (
                "PqRiderSchoolExpiredCheck",
                vec![0xCF, 0x09, 0xD9, 0x7E, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ),
            (
                "PqRankerInfoPacket",
                vec![0x09, 0x07, 0xD7, 0x41, 0, 7, 0, 0, 0xC8, 0x42, 0, 0, 0, 0],
            ),
        ] {
            let request = PacketWriter::named(request_name).into_inner();
            assert_eq!(
                dispatch_packet(&services, &request, &mut context)
                    .await
                    .unwrap(),
                vec![expected]
            );
        }
        assert!(
            dispatch_packet(
                &services,
                PacketWriter::named("LoRqGetRiderItemPacket").as_slice(),
                &mut context,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(matches!(
            dispatch_packet(&services, &hash.to_le_bytes(), &mut context).await,
            Err(LoginSessionError::UnsupportedIdentityPacket { hash: actual })
                if actual == hash
        ));
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn authenticated_dispatch_replies_to_read_only_menu_store_initialization_queries() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_707))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "MenuStoreInitialization")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        store.load_or_create(&identity.nickname).unwrap();
        store
            .update(&identity.nickname, |profile| {
                profile.rider.koin = 0x1122_3344;
                profile.rider.cash = 0x5566_7788;
                profile.rider.tc_cash = 0x99AA_BBCC;
            })
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        let before_revision = store.load_or_create(&identity.nickname).unwrap().revision;

        for (request_name, expected) in [
            (
                "SpRqGetMaxGiftIdPacket",
                vec![0x5A, 0x08, 0xA1, 0x5E, 0, 0, 0, 0],
            ),
            (
                "SpRqKoinBalance",
                vec![0xBC, 0x05, 0x40, 0x2D, 0x44, 0x33, 0x22, 0x11, 0, 0, 0, 0],
            ),
            (
                "PqFavoriteTrackMapGet",
                vec![0x35, 0x08, 0x52, 0x5A, 0, 0, 0, 0],
            ),
            (
                "SpRqGetCashInventoryPacket",
                vec![0x4A, 0x0A, 0x5C, 0x87, 0, 0, 0, 0, 0],
            ),
            (
                "SpRqRemainCashPacket",
                vec![0xB8, 0x07, 0xDB, 0x4F, 0, 0, 0, 0, 0x88, 0x77, 0x66, 0x55],
            ),
            (
                "SpRqRemainTcCashPacket",
                vec![0x6F, 0x08, 0xCE, 0x5F, 99, 0, 0, 0, 0xCC, 0xBB, 0xAA, 0x99],
            ),
        ] {
            let mut request = PacketWriter::named(request_name);
            if request_name == "SpRqKoinBalance" {
                request.write_u8(1);
            }
            assert_eq!(
                dispatch_packet(&services, request.as_slice(), &mut context)
                    .await
                    .unwrap(),
                vec![expected]
            );
        }

        let after = store.load_or_create(&identity.nickname).unwrap();
        assert_eq!(after.revision, before_revision);
        assert_eq!(after.profile.rider.koin, 0x1122_3344);
        assert_eq!(after.profile.rider.cash, 0x5566_7788);
        assert_eq!(after.profile.rider.tc_cash, 0x99AA_BBCC);
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[test]
    #[ignore = "requires external packet traces via P5136_PACKET_TRACE_DIR"]
    fn external_retained_packet_corpus_matches_the_dispatch_domains() {
        let Some(directory) = std::env::var_os("P5136_PACKET_TRACE_DIR") else {
            eprintln!("P5136_PACKET_TRACE_DIR is not set; skipping local corpus audit");
            return;
        };
        let packets = read_retained_rx_packets(Path::new(&directory));
        assert_eq!(packets.len(), 19_496);

        let mut all_hashes = BTreeSet::new();
        let mut tcp_hashes = BTreeSet::new();
        let mut gap_hashes = BTreeSet::new();
        let mut game_slot_type_counts = [0_usize; 17];
        let mut type_twelve_trace_routes = BTreeMap::new();
        for (transport, hash, packet) in &packets {
            all_hashes.insert(*hash);
            match transport.as_str() {
                "TCP" => {
                    tcp_hashes.insert(*hash);
                    assert!(
                        dispatcher_owns_hash(*hash),
                        "TCP hash 0x{hash:08X} has no Rust dispatch domain"
                    );
                    if validate_retained_gap_packet(*hash, packet) {
                        gap_hashes.insert(*hash);
                    }
                    if *hash == GAME_SLOT_PACKET_HASH {
                        let parsed = parse_game_slot_packet(packet).unwrap_or_else(|error| {
                            panic!("retained TCP GameSlot packet failed strict parsing: {error}")
                        });
                        game_slot_type_counts[usize::from(parsed.body().packet_type())] += 1;
                        if let GameSlotBody::ItemOperation(operation) = parsed.body() {
                            *type_twelve_trace_routes
                                .entry((
                                    parsed.player_id(),
                                    parsed.item_or_recipient_mask(),
                                    operation.operation_hash,
                                    operation.state,
                                ))
                                .or_insert(0_usize) += 1;
                        }
                    }
                }
                "UDP" | "P2P" => {
                    parse_routed_udp_packet(packet)
                        .unwrap_or_else(|error| panic!("{transport} 0x{hash:08X}: {error}"));
                }
                other => panic!("unexpected retained transport {other}"),
            }
        }

        assert_eq!(all_hashes.len(), 100);
        assert_eq!(tcp_hashes.len(), 97);
        assert_eq!(
            game_slot_type_counts.iter().sum::<usize>(),
            1_471,
            "every retained TCP GameSlot record must cross the strict parser"
        );
        assert_eq!(game_slot_type_counts[1], 43);
        assert_eq!(game_slot_type_counts[2], 22);
        assert_eq!(game_slot_type_counts[9], 1_337);
        assert_eq!(game_slot_type_counts[10], 38);
        assert_eq!(game_slot_type_counts[11], 1);
        assert_eq!(game_slot_type_counts[12], 30);
        assert_eq!(
            type_twelve_trace_routes,
            BTreeMap::from([
                ((0, 0x0, 0x1090_0367, 2), 1),
                ((0, 0x0, 0x1139_0397, 0), 10),
                ((0, 0x2, 0x1139_0397, 0), 6),
                ((1, 0x1, 0x1090_0367, 2), 1),
                ((1, 0x1, 0x1129_038E, 2), 1),
                ((1, 0x1, 0x1139_0397, 1), 5),
                ((1, 0x1, 0x1D86_04A3, 1), 6),
            ]),
            "retained type-12 masks are peer-only (or zero when solo), not sender-inclusive"
        );
        assert_eq!(
            gap_hashes.len(),
            28,
            "every previously unclassified TCP family must be parsed"
        );
    }

    #[tokio::test]
    async fn authenticated_dispatch_handles_every_captured_read_only_query_without_mutation() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_708))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "CapturedReadOnlyQueries")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        store.load_or_create(&identity.nickname).unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };

        let mut unbound_context = SessionContext::default();
        let current_cmp = CapturedQueryRequest::CurrentCompetition;
        assert!(matches!(
            dispatch_packet(
                &services,
                &current_cmp.request_hash().to_le_bytes(),
                &mut unbound_context,
            )
            .await,
            Err(LoginSessionError::ProfileNotBound)
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        let before_revision = store.load_or_create(&identity.nickname).unwrap().revision;
        for request in CAPTURED_QUERY_REQUESTS {
            for &length in request.observed_lengths() {
                let mut packet = vec![0_u8; length];
                packet[..4].copy_from_slice(&request.request_hash().to_le_bytes());
                if *request == CapturedQueryRequest::EventBuyCount {
                    packet[4..8].copy_from_slice(&4_i32.to_le_bytes());
                }

                let replies = dispatch_packet(&services, &packet, &mut context)
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "{} length {length} was not handled: {error}",
                            request.request_name()
                        )
                    });
                assert_eq!(
                    replies.len(),
                    1,
                    "{} length {length}",
                    request.request_name()
                );
            }
        }

        let malformed = current_cmp.request_hash().to_le_bytes().repeat(2);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::CapturedQueryProtocol(_))
        ));
        assert_eq!(
            store.load_or_create(&identity.nickname).unwrap().revision,
            before_revision
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn packet_hashes_outside_the_covered_corpus_remain_fail_closed() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_710))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "CapturedFailClosed")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        store.load_or_create(&identity.nickname).unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;

        let unknown_hash = 0xDEAD_BEEF_u32;
        assert!(matches!(
            dispatch_packet(&services, &unknown_hash.to_le_bytes(), &mut context).await,
            Err(LoginSessionError::UnsupportedIdentityPacket { hash })
                if hash == unknown_hash
        ));
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn captured_career_and_udp_reconnect_events_are_strict_no_reply_operations() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_711))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "InitializationEvents")
            .await
            .unwrap();
        ProfileStore::new(profile_root.path())
            .load_or_create(&identity.nickname)
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        let career = [
            0x25, 0x0A, 0xCF, 0x86, 0x0A, 0, 0, 0, 1, 0, 0, 0, 1, 0, 0, 0, 8, 0, 1, 0, 0, 0, 1, 0,
            0, 0,
        ];
        let reconnect = PacketWriter::named("PqReportUdpReconnect");

        assert!(
            dispatch_packet(&services, &career, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            dispatch_packet(&services, reconnect.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );

        let mut malformed = reconnect.into_inner();
        malformed.push(0);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::ClientEventProtocol(
                ClientEventProtocolError::TrailingBytes {
                    name: "PqReportUdpReconnect",
                    count: 1,
                }
            ))
        ));
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn every_remaining_captured_telemetry_family_is_a_bounded_no_reply_operation() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_713))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "CapturedTelemetry")
            .await
            .unwrap();
        ProfileStore::new(profile_root.path())
            .load_or_create(&identity.nickname)
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;

        let mut reports = Vec::new();
        let mut ai = vec![0; p5136_core::telemetry_protocol::GAME_AI_REPORT_LENGTH];
        ai[..4].copy_from_slice(&adler32::packet_hash("GameAiReportPacket").to_le_bytes());
        reports.push(ai);

        let mut game = vec![0; p5136_core::telemetry_protocol::GAME_REPORT_LENGTH];
        game[..4].copy_from_slice(&adler32::packet_hash("GameReportPacket").to_le_bytes());
        reports.push(game);

        let mut frame = PacketWriter::named("PcGameClientFramePacket");
        frame.write_u32(126);
        frame.write_u32(145);
        frame.write_u32(140);
        reports.push(frame.into_inner());

        let mut relay = PacketWriter::named("PcGameRequestRelay");
        relay.write_i32(0);
        relay.write_u32(0xA1B7_1C9D);
        reports.push(relay.into_inner());

        reports.push(vec![
            0x0D, 0x09, 0xF0, 0x69, 0x01, 0, 0, 0, 0x07, 0, 0, 0, 0x69, 0, 0x74, 0, 0x65, 0, 0x6D,
            0, 0x55, 0, 0x73, 0, 0x65, 0, 0x14, 0x29, 0xA3, 0x43, 0x1C, 0x69, 0x4C, 0x44, 0x99,
            0x66, 0xB1, 0x41, 0, 0, 0, 0, 1, 0x0A, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0xE0, 0x65, 0, 0,
        ]);
        reports.push(vec![
            0x98, 0x08, 0x40, 0x60, 1, 0, 0, 0, 0xDB, 0xA1, 0x0F, 0x43, 0x03, 0x2C, 0x02, 0x44,
            0x77, 0xCC, 0xD6, 0x41, 0xBD, 0xB4, 0x9A, 0x3F, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        ]);
        for length in p5136_core::telemetry_protocol::UNIDENTIFIED_DRIVING_REPORT_LENGTHS {
            let mut report = vec![0; length];
            report[..4].copy_from_slice(&0x5815_082A_u32.to_le_bytes());
            reports.push(report);
        }

        for report in reports {
            assert!(
                dispatch_packet(&services, &report, &mut context)
                    .await
                    .unwrap()
                    .is_empty()
            );
        }
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn captured_single_player_time_attack_flow_is_durable_and_not_replayable() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_712))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "SinglePlayerFlow")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        let initial = store.load_or_create(&identity.nickname).unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;

        let kart_spec = captured_kart_spec_request();
        let kart_reply = dispatch_packet(&services, &kart_spec, &mut context)
            .await
            .unwrap();
        assert_eq!(kart_reply.len(), 1);
        assert_eq!(
            kart_reply[0].len(),
            p5136_core::single_player_protocol::KART_SPEC_REPLY_LENGTH
        );

        let track = 0x2B1E_038E;
        let start = time_attack_start_request(track, 1);
        let start_reply = dispatch_packet(&services, &start, &mut context)
            .await
            .unwrap();
        assert_eq!(start_reply.len(), 1);
        assert_eq!(
            start_reply[0].len(),
            p5136_core::single_player_protocol::START_TIME_ATTACK_REPLY_LENGTH
        );

        let charged = store.load_or_create(&identity.nickname).unwrap();
        assert_eq!(
            charged.profile.rider.lucci,
            initial.profile.rider.lucci - 1_000
        );
        let active_before_replay = context.active_time_attack().unwrap();
        assert!(matches!(
            dispatch_packet(&services, &start, &mut context).await,
            Err(LoginSessionError::TimeAttackStartWhileActive)
        ));
        let after_replay = store.load_or_create(&identity.nickname).unwrap();
        assert_eq!(after_replay.revision, charged.revision);
        assert_eq!(
            after_replay.profile.rider.lucci,
            charged.profile.rider.lucci
        );
        assert_eq!(context.active_time_attack().unwrap(), active_before_replay);

        let single_start = captured_single_start_request();
        assert!(
            dispatch_packet(&services, &single_start, &mut context)
                .await
                .unwrap()
                .is_empty()
        );

        let use_item = captured_use_item_request();
        assert!(
            dispatch_packet(&services, &use_item, &mut context)
                .await
                .unwrap()
                .is_empty()
        );

        let finish = captured_time_attack_finish_request();
        let finish_reply = dispatch_packet(&services, &finish, &mut context)
            .await
            .unwrap();
        assert_eq!(
            finish_reply,
            vec![
                p5136_core::single_player_protocol::serialize_finish_time_attack_reply(2, 0, 1, 1)
            ]
        );

        let loaded = store.load_or_create(&identity.nickname).unwrap();
        assert_eq!(loaded.profile.rider.track, track);
        assert_eq!(loaded.profile.rider.speed_type, 7);
        assert_eq!(loaded.profile.rider.game_type, 0);
        assert_eq!(loaded.profile.rider.attack_type, 0);
        assert_eq!(loaded.profile.rider.time, 101_731);
        assert_eq!(loaded.profile.rider.rp, p5136_profile::DEFAULT_RP);
        assert_eq!(
            loaded.profile.rider.lucci,
            initial.profile.rider.lucci - 1_000 + 50
        );

        assert!(matches!(
            dispatch_packet(&services, &finish, &mut context).await,
            Err(LoginSessionError::TimeAttackFinishWithoutStart)
        ));

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn scenario_start_is_durable_and_completion_reads_the_bound_revision() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_709))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "ScenarioPersistence")
            .await
            .unwrap();
        let store = ProfileStore::new(profile_root.path());
        let initial = store.load_or_create(&identity.nickname).unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;

        let mut complete = PacketWriter::named(COMPLETE_SCENARIO_REQUEST_NAME);
        complete.write_bytes(&[0; 22]);
        assert_eq!(
            dispatch_packet(&services, complete.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_complete_scenario_reply(0)]
        );
        assert_eq!(
            store.load_or_create(&identity.nickname).unwrap().revision,
            initial.revision
        );

        let scenario_type = 0x0100_0034;
        let mut start = PacketWriter::named(START_SCENARIO_REQUEST_NAME);
        start.write_i32(scenario_type);
        assert_eq!(
            dispatch_packet(&services, start.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_start_scenario_reply(scenario_type)]
        );
        let persisted = store.load_or_create(&identity.nickname).unwrap();
        assert_ne!(persisted.revision, initial.revision);
        assert_eq!(persisted.profile.rider.scenario_type, scenario_type);

        assert_eq!(
            dispatch_packet(&services, complete.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_complete_scenario_reply(scenario_type)]
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shop_buy_aliases_fail_closed_without_ending_the_authenticated_session() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_709))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "ShopFailClosed")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };

        let valid_normal = exact_shop_buy_request(ShopBuyRequest::Normal);
        assert_eq!(valid_normal.len(), 13);
        let mut unbound_context = SessionContext::default();
        assert!(matches!(
            dispatch_packet(&services, &valid_normal, &mut unbound_context).await,
            Err(LoginSessionError::ProfileNotBound)
        ));
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        let expected_failure = serialize_shop_buy_failure();
        assert_eq!(expected_failure.len(), 29);
        assert_eq!(&expected_failure[..5], &[0x01, 0x07, 0x5B, 0x41, 0x01]);
        assert!(expected_failure[5..].iter().all(|byte| *byte == 0));

        let mut context = bind_test_profile(&profiles, &identity).await;
        for request in [ShopBuyRequest::Normal, ShopBuyRequest::ItemPreset] {
            let valid = exact_shop_buy_request(request);
            assert_eq!(
                valid.len(),
                match request {
                    ShopBuyRequest::Normal => 13,
                    ShopBuyRequest::ItemPreset => 15,
                }
            );
            let hash_only = valid[..4].to_vec();
            let truncated = valid[..valid.len() - 1].to_vec();
            let mut trailing = valid.clone();
            trailing.push(0xA5);

            for (packet, should_reply) in [
                (hash_only, false),
                (truncated, false),
                (valid, true),
                (trailing, false),
            ] {
                let expected = if should_reply {
                    vec![expected_failure.clone()]
                } else {
                    Vec::new()
                };
                assert_eq!(
                    dispatch_packet(&services, &packet, &mut context)
                        .await
                        .unwrap(),
                    expected
                );
                assert_eq!(
                    world.authorize_identity(session_id).await.unwrap(),
                    identity
                );

                let server_time = PacketWriter::named("PqServerTime");
                let responses = dispatch_packet(&services, server_time.as_slice(), &mut context)
                    .await
                    .unwrap();
                assert_eq!(responses.len(), 1);
                let mut response = PacketReader::new(&responses[0]);
                assert_eq!(
                    response.read_u32().unwrap(),
                    adler32::packet_hash("PrServerTime")
                );
                let _days_since_1900 = response.read_u16().unwrap();
                assert!(response.read_u16().unwrap() < 24 * 60 * 15);
                assert!(response.remaining().is_empty());
                assert_eq!(
                    world.authorize_identity(session_id).await.unwrap(),
                    identity
                );
            }
        }

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn shop_buy_failure_obeys_stale_generation_and_quiesce_fences() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let peer_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(peer_ip, 49_710))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(peer_ip, 49_711))
            .await
            .unwrap();
        let source_identity = world
            .claim_identity(source, "ShopIdentityFence")
            .await
            .unwrap();
        let mut source_context = bind_test_profile(&profiles, &source_identity).await;
        let source_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: source,
        };
        let request = exact_shop_buy_request(ShopBuyRequest::Normal);

        let token = MigrationToken::new(0x5A13).unwrap();
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
        let completion = world
            .complete_migration(
                destination,
                source_identity.user_no,
                12,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        assert!(matches!(
            dispatch_packet(&source_services, &request, &mut source_context).await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));
        assert_eq!(
            world.authorize_identity(destination).await.unwrap(),
            completion.binding
        );

        let destination_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: destination,
        };
        let mut destination_context = bind_test_profile(&profiles, &completion.binding).await;
        world.quiesce().await.unwrap();
        assert!(matches!(
            dispatch_packet(&destination_services, &request, &mut destination_context).await,
            Err(LoginSessionError::World(
                WorldError::OutboundProductionClosed
            ))
        ));

        world.drain_sessions().await.unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn locked_item_list_dispatch_is_strict_and_returns_one_terminal_packet() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_707))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "LockedItemClassifier")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let request = PacketWriter::named(LOCKED_ITEM_LIST_REQUEST_NAME).into_inner();
        let mut malformed = request.clone();
        malformed.push(0x51);

        let mut unbound_context = SessionContext::default();
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut unbound_context).await,
            Err(LoginSessionError::ItemStateProtocol(
                ItemStateProtocolError::TrailingBytes {
                    name: p5136_core::item_state_protocol::LOCKED_ITEM_GET_REQUEST_NAME,
                    count: 1,
                }
            ))
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        assert_eq!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap(),
            vec![serialize_empty_locked_item_list()]
        );

        let item = FavoriteItemKey::new(3, 1_450, 2);
        let mut update = PacketWriter::named(LOCKED_ITEM_UPDATE_REQUEST_NAME);
        update.write_u8(1);
        update.write_u32(1);
        update.write_u16(item.category());
        update.write_u16(item.item_id());
        update.write_u16(item.serial());
        update.write_u8(1);
        assert!(
            dispatch_packet(&services, update.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap(),
            vec![serialize_locked_item_list(&[item], config.max_login_payload).unwrap()]
        );
        let durable = ProfileStore::new(profile_root.path())
            .reload(&identity.nickname)
            .unwrap();
        assert_eq!(
            durable
                .profile
                .locked_items
                .as_ref()
                .expect("locked state is canonical")
                .as_slice(),
            [item]
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn server_time_dispatch_returns_one_bounded_legacy_clock() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_708))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "ServerTimeClassifier")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;

        // The three corroborating C# handlers do not consume a request body,
        // and no checked-in producer or capture proves hash-only exhaustion.
        // Preserve that bounded compatibility contract instead of inventing a
        // stricter layout.
        for trailing in [&[][..], &[0x51, 0x36][..]] {
            let mut request = PacketWriter::named("PqServerTime");
            request.write_bytes(trailing);
            let responses = dispatch_packet(&services, request.as_slice(), &mut context)
                .await
                .unwrap();
            assert_eq!(responses.len(), 1);

            let mut response = PacketReader::new(&responses[0]);
            assert_eq!(
                response.read_u32().unwrap(),
                adler32::packet_hash("PrServerTime")
            );
            let _days_since_1900 = response.read_u16().unwrap();
            assert!(response.read_u16().unwrap() < 24 * 60 * 15);
            assert!(response.remaining().is_empty());
        }
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn strict_stateless_compatibility_replies_are_direct_and_profile_read_only() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_712))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "StatelessCompat")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let cases = [
            (
                REQUEST_EXTRADATA_REQUEST_NAME,
                serialize_pr_request_extradata(),
            ),
            (
                WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME,
                serialize_pr_web_event_complete_check(),
            ),
        ];

        let mut unbound_context = SessionContext::default();
        for (request_name, _) in &cases {
            let request = PacketWriter::named(request_name).into_inner();
            assert!(matches!(
                dispatch_packet(&services, &request, &mut unbound_context).await,
                Err(LoginSessionError::ProfileNotBound)
            ));

            // For a live admitted identity, exact wire validation precedes
            // packet-specific authorization and the bound-profile fence.
            let mut malformed = request;
            malformed.push(0x51);
            assert!(matches!(
                dispatch_packet(&services, &malformed, &mut unbound_context).await,
                Err(LoginSessionError::StartupProtocol(
                    StartupError::TrailingBytes {
                        name,
                        count: 1
                    }
                )) if name == *request_name
            ));
        }
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        let mut context = bind_test_profile(&profiles, &identity).await;
        let store = ProfileStore::new(profile_root.path());
        let before = store.reload(&identity.nickname).unwrap();
        for (request_name, expected_response) in &cases {
            let request = PacketWriter::named(request_name).into_inner();
            assert_eq!(
                dispatch_packet(&services, &request, &mut context)
                    .await
                    .unwrap(),
                vec![expected_response.clone()]
            );
            assert_eq!(
                world.authorize_identity(session_id).await.unwrap(),
                identity
            );

            let server_time = PacketWriter::named("PqServerTime");
            let follow_up = dispatch_packet(&services, server_time.as_slice(), &mut context)
                .await
                .unwrap();
            assert_eq!(follow_up.len(), 1);
            assert_eq!(
                &follow_up[0][..4],
                &adler32::packet_hash("PrServerTime").to_le_bytes()
            );

            let mut trailing = request;
            trailing.push(0x51);
            assert!(matches!(
                dispatch_packet(&services, &trailing, &mut context).await,
                Err(LoginSessionError::StartupProtocol(
                    StartupError::TrailingBytes {
                        name,
                        count: 1
                    }
                )) if name == *request_name
            ));
        }

        let after = store.reload(&identity.nickname).unwrap();
        assert_eq!(after.revision, before.revision);
        assert_eq!(after.profile, before.profile);

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn strict_stateless_compatibility_replies_obey_generation_and_quiesce_fences() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let peer_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(peer_ip, 49_713))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(peer_ip, 49_714))
            .await
            .unwrap();
        let source_identity = world
            .claim_identity(source, "StatelessCompatFence")
            .await
            .unwrap();
        let mut source_context = bind_test_profile(&profiles, &source_identity).await;
        let source_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: source,
        };

        let token = MigrationToken::new(0x5A14).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 13,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                source_identity.user_no,
                13,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        let web_event = PacketWriter::named(WEB_EVENT_COMPLETE_CHECK_REQUEST_NAME).into_inner();
        let mut malformed_web_event = web_event.clone();
        malformed_web_event.push(0x51);
        // Global identity-operation admission rejects stale ownership before
        // the packet-specific parser is reached.
        assert!(matches!(
            dispatch_packet(&source_services, &malformed_web_event, &mut source_context).await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));
        assert!(matches!(
            dispatch_packet(&source_services, &web_event, &mut source_context).await,
            Err(LoginSessionError::World(WorldError::Identity(
                IdentityError::StaleSession(id)
            ))) if id == source
        ));
        assert_eq!(
            world.authorize_identity(destination).await.unwrap(),
            completion.binding
        );

        let destination_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: destination,
        };
        let mut destination_context = bind_test_profile(&profiles, &completion.binding).await;
        let extradata = PacketWriter::named(REQUEST_EXTRADATA_REQUEST_NAME).into_inner();
        world.quiesce().await.unwrap();
        let mut malformed_extradata = extradata.clone();
        malformed_extradata.push(0x51);
        // The producer barrier likewise closes before packet-specific parsing.
        assert!(matches!(
            dispatch_packet(
                &destination_services,
                &malformed_extradata,
                &mut destination_context
            )
            .await,
            Err(LoginSessionError::World(
                WorldError::OutboundProductionClosed
            ))
        ));
        assert!(matches!(
            dispatch_packet(&destination_services, &extradata, &mut destination_context).await,
            Err(LoginSessionError::World(
                WorldError::OutboundProductionClosed
            ))
        ));
        world.drain_sessions().await.unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn club_and_item_state_global_fences_precede_packet_specific_parsing() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let peer_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(peer_ip, 49_717))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(peer_ip, 49_718))
            .await
            .unwrap();
        let source_identity = world
            .claim_identity(source, "ClubQueryFence")
            .await
            .unwrap();
        let mut source_context = bind_test_profile(&profiles, &source_identity).await;
        let source_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: source,
        };
        let request = exact_club_query_request(ClubQueryRequest::GetClubListCount);
        let mut malformed = request.clone();
        malformed.push(0x51);
        let item_request = exact_favorite_item_get();
        let mut malformed_item = item_request.clone();
        malformed_item.push(0x51);

        let token = MigrationToken::new(0x5A15).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 14,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                source_identity.user_no,
                14,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        for packet in [&request, &malformed, &item_request, &malformed_item] {
            assert!(matches!(
                dispatch_packet(&source_services, packet, &mut source_context).await,
                Err(LoginSessionError::World(WorldError::Identity(
                    IdentityError::StaleSession(id)
                ))) if id == source
            ));
        }

        let destination_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: destination,
        };
        let mut destination_context = bind_test_profile(&profiles, &completion.binding).await;
        world.quiesce().await.unwrap();
        for packet in [&request, &malformed, &item_request, &malformed_item] {
            assert!(matches!(
                dispatch_packet(&destination_services, packet, &mut destination_context).await,
                Err(LoginSessionError::World(
                    WorldError::OutboundProductionClosed
                ))
            ));
        }

        world.drain_sessions().await.unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn club_queries_are_strict_read_only_and_preserve_the_live_session() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_717))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "ClubQueryCaller")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };

        let requests = [
            ClubQueryRequest::CheckMyClubState,
            ClubQueryRequest::GetUserWaitingJoinClub,
            ClubQueryRequest::CheckCreateClubCondition,
            ClubQueryRequest::GetClubListCount,
            ClubQueryRequest::GetClubWaitingCrewCount,
        ];
        let mut unbound_context = SessionContext::default();
        for kind in requests {
            assert!(matches!(
                dispatch_packet(
                    &services,
                    &exact_club_query_request(kind),
                    &mut unbound_context
                )
                .await,
                Err(LoginSessionError::ProfileNotBound)
            ));
        }

        let mut trailing = exact_club_query_request(ClubQueryRequest::CheckMyClubState);
        trailing.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &trailing, &mut unbound_context).await,
            Err(LoginSessionError::ClubQueryProtocol(
                ClubQueryProtocolError::TrailingBytes {
                    name,
                    count: 1
                }
            )) if name == ClubQueryRequest::CheckMyClubState.request_name()
        ));

        let mut zero_code =
            PacketWriter::named(ClubQueryRequest::GetClubWaitingCrewCount.request_name());
        zero_code.write_u32(0);
        assert!(matches!(
            dispatch_packet(&services, zero_code.as_slice(), &mut unbound_context).await,
            Err(LoginSessionError::ClubQueryProtocol(
                ClubQueryProtocolError::ZeroClubCode
            ))
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        let in_memory_before = context.profile_for(&identity).unwrap().clone();
        let store = ProfileStore::new(profile_root.path());
        let durable_before = store.reload(&identity.nickname).unwrap();

        for _ in 0..2 {
            for kind in requests {
                assert_eq!(
                    dispatch_packet(&services, &exact_club_query_request(kind), &mut context)
                        .await
                        .unwrap(),
                    vec![expected_club_query_reply(kind)],
                    "{}",
                    kind.request_name()
                );
            }
        }

        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );
        assert_eq!(context.profile_for(&identity).unwrap(), &in_memory_before);

        let server_time = PacketWriter::named("PqServerTime");
        let follow_up = dispatch_packet(&services, server_time.as_slice(), &mut context)
            .await
            .unwrap();
        assert_eq!(follow_up.len(), 1);
        assert_eq!(
            &follow_up[0][..4],
            &adler32::packet_hash("PrServerTime").to_le_bytes()
        );

        let durable_after = store.reload(&identity.nickname).unwrap();
        assert_eq!(durable_after.revision, durable_before.revision);
        assert_eq!(durable_after.profile, durable_before.profile);

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn delete_and_unlock_are_strict_read_only_no_reply_capabilities() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_719))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "UnsupportedItemState")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let requests = [
            exact_delete_item_request(FavoriteItemKey::new(3, 1_450, 2), 1),
            exact_unlock_item_request(),
        ];

        let mut unbound = SessionContext::default();
        for request in &requests {
            assert!(matches!(
                dispatch_packet(&services, request, &mut unbound).await,
                Err(LoginSessionError::ProfileNotBound)
            ));
        }
        let mut malformed = requests[0].clone();
        malformed.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut unbound).await,
            Err(LoginSessionError::ItemStateProtocol(
                ItemStateProtocolError::TrailingBytes { count: 1, .. }
            ))
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        let in_memory_before = context.profile_for(&identity).unwrap().clone();
        let store = ProfileStore::new(profile_root.path());
        let durable_before = store.reload(&identity.nickname).unwrap();
        for _ in 0..2 {
            for request in &requests {
                assert_eq!(
                    dispatch_packet(&services, request, &mut context)
                        .await
                        .unwrap(),
                    Vec::<Vec<u8>>::new()
                );
            }
        }
        let durable_after = store.reload(&identity.nickname).unwrap();
        assert_eq!(durable_after.revision, durable_before.revision);
        assert_eq!(durable_after.profile, durable_before.profile);
        assert_eq!(context.profile_for(&identity).unwrap(), &in_memory_before);

        let follow_up = PacketWriter::named("PqServerTime");
        assert_eq!(
            dispatch_packet(&services, follow_up.as_slice(), &mut context)
                .await
                .unwrap()
                .len(),
            1
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "the regression covers two batches, one-way replies, durable replay, full Get projection, and bound-cache refresh"
    )]
    async fn favorite_updates_are_atomic_durable_and_get_returns_the_full_snapshot() {
        let profile_root = tempfile::tempdir().unwrap();
        seed_canonical_favorite_profile(profile_root.path(), "FavoriteRoundtrip");
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_720))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "FavoriteRoundtrip")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        let cached_before = context.profile_for(&identity).unwrap().clone();
        assert!(
            cached_before
                .favorite_items
                .as_ref()
                .is_some_and(FavoriteItems::is_empty)
        );

        let first_items = (0..200_u16)
            .map(|serial| FavoriteItemKey::new(3, 1_450, serial))
            .collect::<Vec<_>>();
        let first_records = first_items
            .iter()
            .copied()
            .map(|item| (item, 1))
            .collect::<Vec<_>>();
        assert_eq!(
            dispatch_packet(
                &services,
                &exact_favorite_item_update(&first_records),
                &mut context
            )
            .await
            .unwrap(),
            Vec::<Vec<u8>>::new()
        );

        let last = FavoriteItemKey::new(3, 1_450, 200);
        let second = exact_favorite_item_update(&[(last, 1)]);
        assert!(
            dispatch_packet(&services, &second, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        let mut expected = first_items;
        expected.push(last);
        assert_eq!(
            dispatch_packet(&services, &exact_favorite_item_get(), &mut context)
                .await
                .unwrap(),
            vec![
                serialize_favorite_item_list(&expected, config.max_login_payload)
                    .expect("201-item snapshot fits")
            ]
        );

        let store = ProfileStore::new(profile_root.path());
        let durable = store.reload(&identity.nickname).unwrap();
        assert_eq!(
            favorite_item_snapshot(durable.profile.favorite_items.as_ref()),
            expected
        );
        let durable_revision = durable.revision;
        assert!(
            dispatch_packet(&services, &second, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            store.reload(&identity.nickname).unwrap().revision,
            durable_revision,
            "idempotent add replay must not publish a revision"
        );
        let mut expected_cached = cached_before;
        expected_cached.favorite_items = Some(
            FavoriteItems::try_from_items(expected.iter().copied())
                .expect("wire snapshot is canonical"),
        );
        assert_eq!(
            context.profile_for(&identity).unwrap(),
            &expected_cached,
            "favorite writes must patch the bound favorite projection without replacing others"
        );
        assert_eq!(
            context
                .profile
                .as_ref()
                .expect("profile remains bound")
                .profile
                .revision,
            durable_revision
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn malformed_or_over_cap_favorite_batches_never_partially_commit() {
        let profile_root = tempfile::tempdir().unwrap();
        seed_canonical_favorite_profile(profile_root.path(), "FavoriteAtomicity");
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig {
            max_login_payload: 16,
            ..ServerConfig::default()
        };
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_721))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "FavoriteAtomicity")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        let first = FavoriteItemKey::new(3, 1_450, 1);
        let second = FavoriteItemKey::new(3, 1_450, 2);
        dispatch_packet(
            &services,
            &exact_favorite_item_update(&[(first, 1)]),
            &mut context,
        )
        .await
        .unwrap();
        let store = ProfileStore::new(profile_root.path());
        let before = store.reload(&identity.nickname).unwrap();

        let malformed = exact_favorite_item_update(&[(second, 1), (first, 3)]);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::ItemStateProtocol(
                ItemStateProtocolError::InvalidFavoriteOperation {
                    index: 1,
                    actual: 3
                }
            ))
        ));
        let after_malformed = store.reload(&identity.nickname).unwrap();
        assert_eq!(after_malformed.revision, before.revision);
        assert_eq!(after_malformed.profile, before.profile);

        let over_cap = exact_favorite_item_update(&[(second, 1)]);
        let error = dispatch_packet(&services, &over_cap, &mut context)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            LoginSessionError::FavoriteItemPersistence(FavoriteItemPersistError::State(
                FavoriteItemStateError::TooManyItems {
                    count: 2,
                    maximum: 1
                }
            ))
        ));
        let after_cap = store.reload(&identity.nickname).unwrap();
        assert_eq!(after_cap.revision, before.revision);
        assert_eq!(after_cap.profile, before.profile);

        let replacement = exact_favorite_item_update(&[(second, 1), (first, 2)]);
        assert!(
            dispatch_packet(&services, &replacement, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            dispatch_packet(&services, &exact_favorite_item_get(), &mut context)
                .await
                .unwrap(),
            vec![
                serialize_favorite_item_list(&[second], config.max_login_payload)
                    .expect("one record fits")
            ]
        );

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rider_info_lookup_fails_closed_without_reading_or_creating_remote_profiles() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_715))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "RiderInfoCaller")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let request = exact_get_rider_info_request("OfflineTarget", u8::MAX);
        assert_eq!(
            request.len(),
            17 + 2 * "OfflineTarget".encode_utf16().count()
        );

        let mut unbound_context = SessionContext::default();
        assert!(matches!(
            dispatch_packet(&services, &request, &mut unbound_context).await,
            Err(LoginSessionError::ProfileNotBound)
        ));

        // Exact wire validation precedes the packet-specific profile fence for
        // an otherwise live, globally admitted identity.
        let mut trailing = request.clone();
        trailing.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &trailing, &mut unbound_context).await,
            Err(LoginSessionError::RiderInfoProtocol(
                RiderInfoProtocolError::TrailingBytes {
                    name: GET_RIDER_INFO_REQUEST_NAME,
                    count: 1
                }
            ))
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        let in_memory_before = context.profile_for(&identity).unwrap().clone();
        let store = ProfileStore::new(profile_root.path());
        let durable_before = store.reload(&identity.nickname).unwrap();

        assert_eq!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap(),
            vec![serialize_get_rider_info_failure()]
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );
        assert_eq!(context.profile_for(&identity).unwrap(), &in_memory_before);

        let server_time = PacketWriter::named("PqServerTime");
        let follow_up = dispatch_packet(&services, server_time.as_slice(), &mut context)
            .await
            .unwrap();
        assert_eq!(follow_up.len(), 1);
        assert_eq!(
            &follow_up[0][..4],
            &adler32::packet_hash("PrServerTime").to_le_bytes()
        );

        let durable_after = store.reload(&identity.nickname).unwrap();
        assert_eq!(durable_after.revision, durable_before.revision);
        assert_eq!(durable_after.profile, durable_before.profile);

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rider_school_start_is_strict_canonical_and_profile_read_only() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(4).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_716))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "SchoolStarter")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let request = exact_start_rider_school_request(0xA5);
        assert_eq!(request.len(), 5);

        let mut unbound_context = SessionContext::default();
        assert!(matches!(
            dispatch_packet(&services, &request, &mut unbound_context).await,
            Err(LoginSessionError::ProfileNotBound)
        ));
        assert!(matches!(
            dispatch_packet(&services, &request[..4], &mut unbound_context).await,
            Err(LoginSessionError::StartupProtocol(StartupError::Packet(
                PacketError::Truncated { .. }
            )))
        ));
        let mut trailing = request.clone();
        trailing.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &trailing, &mut unbound_context).await,
            Err(LoginSessionError::StartupProtocol(
                StartupError::TrailingBytes {
                    name: START_RIDER_SCHOOL_REQUEST_NAME,
                    count: 1
                }
            ))
        ));

        let mut context = bind_test_profile(&profiles, &identity).await;
        let in_memory_before = context.profile_for(&identity).unwrap().clone();
        let store = ProfileStore::new(profile_root.path());
        let durable_before = store.reload(&identity.nickname).unwrap();
        let expected = serialize_pr_start_rider_school().unwrap();
        assert_eq!(expected.len(), 240);
        assert_eq!(expected[4], 1);

        assert_eq!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap(),
            vec![expected]
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );
        assert_eq!(context.profile_for(&identity).unwrap(), &in_memory_before);

        let server_time = PacketWriter::named("PqServerTime");
        let follow_up = dispatch_packet(&services, server_time.as_slice(), &mut context)
            .await
            .unwrap();
        assert_eq!(follow_up.len(), 1);
        assert_eq!(
            &follow_up[0][..4],
            &adler32::packet_hash("PrServerTime").to_le_bytes()
        );

        let durable_after = store.reload(&identity.nickname).unwrap();
        assert_eq!(durable_after.revision, durable_before.revision);
        assert_eq!(durable_after.profile, durable_before.profile);

        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn rider_lookup_and_school_start_obey_generation_and_quiesce_fences() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let peer_ip = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(peer_ip, 49_717))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(peer_ip, 49_718))
            .await
            .unwrap();
        let source_identity = world
            .claim_identity(source, "RiderBoundaryFence")
            .await
            .unwrap();
        let mut source_context = bind_test_profile(&profiles, &source_identity).await;
        let source_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: source,
        };

        let token = MigrationToken::new(0x5A15).unwrap();
        world
            .begin_migration(
                source,
                ChannelBinding {
                    channel_id: 14,
                    game_type: 67,
                },
                token,
                Instant::now(),
            )
            .await
            .unwrap();
        let completion = world
            .complete_migration(
                destination,
                source_identity.user_no,
                14,
                token,
                Instant::now(),
            )
            .await
            .unwrap();

        let rider_info = exact_get_rider_info_request("FenceTarget", 1);
        let mut malformed_rider_info = rider_info.clone();
        malformed_rider_info.push(0x51);
        let rider_school = exact_start_rider_school_request(1);
        let mut malformed_rider_school = rider_school.clone();
        malformed_rider_school.push(0x51);
        let requests = [
            rider_info,
            malformed_rider_info,
            rider_school,
            malformed_rider_school,
        ];

        // Global admission rejects stale ownership before either strict parser
        // can inspect a well-formed or malformed packet.
        for request in &requests {
            assert!(matches!(
                dispatch_packet(&source_services, request, &mut source_context).await,
                Err(LoginSessionError::World(WorldError::Identity(
                    IdentityError::StaleSession(id)
                ))) if id == source
            ));
        }
        assert_eq!(
            world.authorize_identity(destination).await.unwrap(),
            completion.binding
        );

        let destination_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: destination,
        };
        let mut destination_context = bind_test_profile(&profiles, &completion.binding).await;
        world.quiesce().await.unwrap();

        // The producer barrier likewise closes before either parser runs.
        for request in &requests {
            assert!(matches!(
                dispatch_packet(&destination_services, request, &mut destination_context).await,
                Err(LoginSessionError::World(
                    WorldError::OutboundProductionClosed
                ))
            ));
        }

        world.drain_sessions().await.unwrap();
        profile_runtime.shutdown().await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn endpoint_reports_persist_only_p2p_port_and_keep_game_udp_isolated() {
        fn endpoint_packet(name: &str, claimed_ip: [u8; 4], port: u16) -> Vec<u8> {
            let mut packet = PacketWriter::named(name);
            packet.write_bytes(&claimed_ip);
            packet.write_u16(port);
            packet.into_inner()
        }

        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_707))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "EndpointReporter")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = bind_test_profile(&profiles, &identity).await;
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 0);

        let first = endpoint_packet("ChClientP2pAddrPacket", [203, 0, 113, 200], 45_136);
        assert!(
            dispatch_packet(&services, &first, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 45_136);
        let stored = ProfileStore::new(profile_root.path())
            .reload(&identity.nickname)
            .unwrap();
        assert_eq!(stored.profile.rider.p2p_port, 45_136);
        let first_revision = stored.revision.unwrap();

        let idempotent = endpoint_packet("ChClientP2pAddrPacket", [198, 51, 100, 99], 45_136);
        dispatch_packet(&services, &idempotent, &mut context)
            .await
            .unwrap();
        assert_eq!(
            ProfileStore::new(profile_root.path())
                .reload(&identity.nickname)
                .unwrap()
                .revision,
            Some(first_revision),
            "a fresh same-port report installs generation authority without inventing a revision"
        );

        let game_udp = endpoint_packet("ChClientUdpAddrPacket", [192, 0, 2, 88], 49_999);
        dispatch_packet(&services, &game_udp, &mut context)
            .await
            .unwrap();
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 45_136);
        assert_eq!(
            ProfileStore::new(profile_root.path())
                .reload(&identity.nickname)
                .unwrap()
                .revision,
            Some(first_revision)
        );

        let mut malformed = first.clone();
        malformed.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::ChannelProtocol(
                p5136_core::channel::ChannelError::TrailingBytes { .. }
            ))
        ));
        assert_eq!(
            ProfileStore::new(profile_root.path())
                .reload(&identity.nickname)
                .unwrap()
                .revision,
            Some(first_revision)
        );

        let clear = endpoint_packet("ChClientP2pAddrPacket", [10, 0, 0, 1], 0);
        dispatch_packet(&services, &clear, &mut context)
            .await
            .unwrap();
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 0);
        let cleared = ProfileStore::new(profile_root.path())
            .reload(&identity.nickname)
            .unwrap();
        assert_eq!(cleared.profile.rider.p2p_port, 0);
        assert!(cleared.revision.unwrap() > first_revision);

        profile_runtime.shutdown().await.unwrap();
        world.drain_myroom_completions().await.unwrap();
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
        profile.rider.p2p_port = 48_888;
        let ipv6_slot = myroom_player_slot_from_profile(&ipv6_identity, &profile);
        assert_eq!(ipv6_slot.p2p_address, Ipv4Addr::UNSPECIFIED);
        assert_eq!(ipv6_slot.p2p_port, 0);

        let mapped_address = Ipv4Addr::new(203, 0, 113, 88);
        let mapped_identity = IdentityBinding {
            source_ip: IpAddr::V6(mapped_address.to_ipv6_mapped()),
            ..rider.identity.clone()
        };
        let mapped_slot = myroom_player_slot_from_profile(&mapped_identity, &profile);
        assert_eq!(mapped_slot.p2p_address, mapped_address);
        assert_eq!(mapped_slot.p2p_port, 48_888);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn profile_binding_preserves_p2p_port_only_for_the_exact_generation() {
        fn snapshot(port: i32, revision: u64) -> super::ProfileSnapshot {
            let mut profile = Profile::default();
            profile.rider.p2p_port = port;
            super::ProfileSnapshot {
                profile,
                revision: Some(revision),
                source_path: std::path::PathBuf::from(format!("Rider.v{revision}.json")),
            }
        }

        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let rider = register_lobby_session(&world, "GenerationPort", 49_737).await;
        let identity = rider.identity;
        let mut context = SessionContext::default();

        context.bind_profile(identity.clone(), snapshot(45_000, 1));
        assert_eq!(
            context.profile_for(&identity).unwrap().rider.p2p_port,
            45_000
        );
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 0);

        context.profile.as_mut().unwrap().reported_p2p_port = 45_136;
        context.bind_profile(identity.clone(), snapshot(46_000, 2));
        assert_eq!(
            context.profile_for(&identity).unwrap().rider.p2p_port,
            46_000
        );
        assert_eq!(context.reported_p2p_port_for(&identity).unwrap(), 45_136);

        let replacement = IdentityBinding {
            owner: SessionId::new(identity.owner.get() + 10),
            ..identity
        };
        context.bind_profile(replacement.clone(), snapshot(47_000, 3));
        assert_eq!(
            context.profile_for(&replacement).unwrap().rider.p2p_port,
            47_000
        );
        assert_eq!(context.reported_p2p_port_for(&replacement).unwrap(), 0);

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "one dispatch fixture proves self bootstrap, the captured empty-password field, public visitor entry, password redaction, and actor-owned packet ordering"
    )]
    async fn myroom_direct_entry_dispatch_bootstraps_self_and_redacts_visitor_secrets() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (direct_owner_session, _direct_owner_cancelled, mut direct_owner_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_730),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let direct_owner = world
            .claim_identity(direct_owner_session, "DispatchDirectOwner")
            .await
            .unwrap();
        let (direct_visitor_session, _direct_visitor_cancelled, mut direct_visitor_outbound) =
            world
                .register_login_session(
                    SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_731),
                    WireOperationGate::new(),
                )
                .await
                .unwrap();
        let direct_visitor = world
            .claim_identity(direct_visitor_session, "DispatchDirectVisitor")
            .await
            .unwrap();

        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut owner_profile = Profile::default();
        owner_profile.rider.p2p_port = 41_360;
        owner_profile.rider.rp = 51_360;
        owner_profile.rider.club_name = "DirectEntryClub".to_owned();
        owner_profile.my_room.my_room = 36;
        owner_profile.my_room.my_room_bgm = 8;
        owner_profile.my_room.use_room_pwd = 0;
        owner_profile.my_room.use_item_pwd = 1;
        owner_profile.my_room.talk_lock = 0;
        owner_profile.my_room.room_pwd = "room-dispatch-secret".to_owned();
        owner_profile.my_room.item_pwd = "item-dispatch-secret".to_owned();
        store.save(&direct_owner.nickname, &owner_profile).unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut direct_owner_context = bind_test_profile(&profiles, &direct_owner).await;
        let mut direct_visitor_context = bind_test_profile(&profiles, &direct_visitor).await;
        let visitor_profile = store
            .load_or_create(&direct_visitor.nickname)
            .unwrap()
            .profile;
        let config = ServerConfig::default();
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: direct_owner_session,
        };
        let visitor_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: direct_visitor_session,
        };

        let mut self_entry = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        self_entry.write_utf16("dispatchdirectowner").unwrap();
        self_entry.write_utf16("").unwrap();
        assert!(
            dispatch_packet(
                &owner_services,
                &self_entry.into_inner(),
                &mut direct_owner_context,
            )
            .await
            .unwrap()
            .is_empty()
        );
        let owner_info = owner_profile.my_room.try_to_protocol_info().unwrap();
        let mut owner_slots: [MyRoomSlot; MYROOM_SLOT_COUNT] =
            array::from_fn(|_| MyRoomSlot::Empty);
        owner_slots[0] = MyRoomSlot::Player(
            myroom_profile_presentation(&owner_profile)
                .with_p2p_port(0)
                .player_for(&direct_owner),
        );
        assert_eq!(
            direct_owner_outbound.try_recv().unwrap().into_packets(),
            vec![
                serialize_enter_reply(
                    &direct_owner.nickname,
                    EnterMyRoomStatus::Success,
                    &owner_info,
                )
                .unwrap(),
                serialize_slot_data(&owner_slots).unwrap(),
            ]
        );

        let mut visitor_entry = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        visitor_entry.write_utf16("DISPATCHDIRECTOWNER").unwrap();
        visitor_entry.write_utf16("").unwrap();
        assert!(
            dispatch_packet(
                &visitor_services,
                &visitor_entry.into_inner(),
                &mut direct_visitor_context,
            )
            .await
            .unwrap()
            .is_empty()
        );
        let mut redacted = owner_info.clone();
        redacted.room_password.clear();
        redacted.item_password.clear();
        let mut current_slots = owner_slots;
        current_slots[1] = MyRoomSlot::Player(myroom_player_slot_from_profile(
            &direct_visitor,
            &visitor_profile,
        ));
        assert_eq!(
            direct_visitor_outbound.try_recv().unwrap().into_packets(),
            vec![
                serialize_enter_reply(
                    &direct_owner.nickname,
                    EnterMyRoomStatus::Success,
                    &redacted,
                )
                .unwrap(),
                serialize_slot_data(&current_slots).unwrap(),
            ]
        );
        assert_eq!(
            direct_owner_outbound.try_recv().unwrap().into_packets(),
            vec![serialize_slot_data(&current_slots).unwrap()]
        );
        assert_eq!(redacted.use_item_password, owner_info.use_item_password);
        assert_eq!(redacted.talk_lock, owner_info.talk_lock);
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[allow(
        clippy::too_many_lines,
        reason = "the dispatch fixture keeps prompt, mismatch, successful entry, redaction, and exact fanout assertions in one protected-room lifecycle"
    )]
    async fn protected_myroom_direct_entry_prompts_until_the_exact_password_matches() {
        let protected = MyRoomInfo {
            use_room_password: 1,
            room_password: "room dispatch secret".to_owned(),
            use_item_password: 1,
            item_password: "item dispatch secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(protected.clone());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (outsider_session, _outsider_cancelled, mut outsider_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_745),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let outsider = world
            .claim_identity(outsider_session, "PasswordDispatchVisitor")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &outsider).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: outsider_session,
        };

        let mut request = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        request.write_utf16(&owner.identity.nickname).unwrap();
        request.write_utf16("").unwrap();
        assert!(
            dispatch_packet(&services, request.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            outsider_outbound.try_recv().unwrap().into_packets(),
            vec![serialize_password_enter_myroom_command(&owner.identity.nickname).unwrap()]
        );
        assert_eq!(
            world.myroom_session_view(outsider_session).await.unwrap(),
            None
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        let mut request = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        request.write_utf16(&owner.identity.nickname).unwrap();
        request.write_utf16("wrong password").unwrap();
        assert!(
            dispatch_packet(&services, request.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            outsider_outbound.try_recv().unwrap().into_packets(),
            vec![serialize_enter_error(EnterMyRoomStatus::PasswordMismatch).unwrap()]
        );
        assert_eq!(
            world.myroom_session_view(outsider_session).await.unwrap(),
            None
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        let mut request = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        request.write_utf16(&owner.identity.nickname).unwrap();
        request.write_utf16("room dispatch secret").unwrap();
        assert!(
            dispatch_packet(&services, request.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        let outsider_packets = outsider_outbound.try_recv().unwrap().into_packets();
        assert_eq!(outsider_packets.len(), 2);
        let mut redacted = protected;
        redacted.room_password.clear();
        redacted.item_password.clear();
        assert_eq!(
            outsider_packets[0],
            serialize_enter_reply(
                &owner.identity.nickname,
                EnterMyRoomStatus::Success,
                &redacted,
            )
            .unwrap()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![outsider_packets[1].clone()]
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![outsider_packets[1].clone()]
        );
        let view = world
            .myroom_session_view(outsider_session)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(view.info(), &redacted);
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        assert!(outsider_outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_reentry_dispatch_bootstraps_self_from_the_bound_profile() {
        let (world, actor) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_740),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "DispatchReentrySelf")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let profile = store.load_or_create(&identity.nickname).unwrap().profile;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };
        let request = PacketWriter::named(REENTER_MYROOM_REQUEST_NAME).into_inner();

        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty()
        );

        let info = profile.my_room.try_to_protocol_info().unwrap();
        let mut slots: [MyRoomSlot; MYROOM_SLOT_COUNT] = array::from_fn(|_| MyRoomSlot::Empty);
        slots[0] = MyRoomSlot::Player(myroom_player_slot_from_profile(&identity, &profile));
        assert_eq!(
            outbound.try_recv().unwrap().into_packets(),
            vec![
                serialize_enter_reply(&identity.nickname, EnterMyRoomStatus::Success, &info)
                    .unwrap(),
                serialize_slot_data(&slots).unwrap(),
            ]
        );

        shutdown_spawned_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_reentry_current_membership_ignores_invalid_self_fallback_info() {
        let owner_info = MyRoomInfo {
            room_id: 5136,
            room_password: "owner-room-secret".to_owned(),
            item_password: "owner-item-secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(owner_info.clone());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut profile = Profile::default();
        profile.my_room.room_pwd = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        store.save(&visitor.identity.nickname, &profile).unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        let request = PacketWriter::named(REENTER_MYROOM_REQUEST_NAME).into_inner();

        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty(),
            "current authoritative membership must not consume invalid self-bootstrap data"
        );

        let packets = visitor.outbound.try_recv().unwrap().into_packets();
        assert_eq!(packets.len(), 2);
        let mut redacted = owner_info;
        redacted.room_password.clear();
        redacted.item_password.clear();
        assert_eq!(
            packets[0],
            serialize_enter_reply(
                &owner.identity.nickname,
                EnterMyRoomStatus::Success,
                &redacted,
            )
            .unwrap()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![packets[1].clone()]
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_reentry_invalid_self_fallback_is_typed_without_stopping_the_actor() {
        let (world, actor) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_744),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "InvalidReentryFallback")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut profile = Profile::default();
        profile.my_room.item_pwd = "x".repeat(MAX_MYROOM_PASSWORD_UTF16_UNITS + 1);
        store.save(&identity.nickname, &profile).unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };
        let request = PacketWriter::named(REENTER_MYROOM_REQUEST_NAME).into_inner();

        assert!(matches!(
            dispatch_packet(&services, &request, &mut context).await,
            Err(LoginSessionError::World(
                WorldError::MyRoomEntrySelfInfoInvalid {
                    session: actual,
                    source: MyRoomProtocolError::StringTooLong {
                        field: "MyRoom item password",
                        ..
                    },
                }
            )) if actual == session
        ));
        assert!(outbound.try_recv().is_err());
        assert_eq!(world.myroom_session_view(session).await.unwrap(), None);
        assert_eq!(world.session_count().await.unwrap(), 1);

        shutdown_spawned_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_random_entry_dispatch_uses_the_only_eligible_public_room() {
        let owner_info = MyRoomInfo {
            room_id: 5136,
            use_item_password: 1,
            room_password: "public-room-raw-secret".to_owned(),
            item_password: "public-item-raw-secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(owner_info.clone());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_741),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "DispatchRandomVisitor")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };
        let request = PacketWriter::named(ENTER_RANDOM_MYROOM_REQUEST_NAME).into_inner();

        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty()
        );

        let packets = outbound.try_recv().unwrap().into_packets();
        assert_eq!(packets.len(), 2);
        let mut redacted = owner_info;
        redacted.room_password.clear();
        redacted.item_password.clear();
        assert_eq!(
            packets[0],
            serialize_enter_reply(
                &owner.identity.nickname,
                EnterMyRoomStatus::Success,
                &redacted,
            )
            .unwrap()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![packets[1].clone()]
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![packets[1].clone()]
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_random_entry_dispatch_reports_no_available_room() {
        let (world, actor) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_742),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "DispatchRandomAlone")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };
        let request = PacketWriter::named(ENTER_RANDOM_MYROOM_REQUEST_NAME).into_inner();

        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            outbound.try_recv().unwrap().into_packets(),
            vec![serialize_enter_error(EnterMyRoomStatus::NoAvailableRoom).unwrap()]
        );
        assert_eq!(world.myroom_session_view(session).await.unwrap(), None);

        shutdown_spawned_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_check_password_dispatch_is_strict_and_uses_the_owner_item_password() {
        let protected = MyRoomInfo {
            use_item_password: 1,
            item_password: "item dispatch secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(protected);
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };

        for password_kind in 0..=3 {
            for (password, status) in [
                ("", CheckPasswordStatus::PasswordRequired),
                ("wrong password", CheckPasswordStatus::WrongPassword),
                ("item dispatch secret", CheckPasswordStatus::Success),
            ] {
                let mut request = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
                request.write_i32(password_kind);
                request.write_utf16(password).unwrap();
                assert_eq!(
                    dispatch_packet(&services, request.as_slice(), &mut context)
                        .await
                        .unwrap(),
                    vec![serialize_check_password_reply(password_kind, status)]
                );
                assert!(owner.outbound.try_recv().is_err());
                assert!(visitor.outbound.try_recv().is_err());
            }
        }

        let mut unsupported = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
        unsupported.write_i32(-7);
        unsupported.write_utf16("item dispatch secret").unwrap();
        assert_eq!(
            dispatch_packet(&services, unsupported.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_check_password_reply(
                -7,
                CheckPasswordStatus::Unsupported,
            )]
        );

        let mut missing_password = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
        missing_password.write_i32(0);
        assert!(matches!(
            dispatch_packet(&services, missing_password.as_slice(), &mut context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(_)
            ))
        ));
        let mut malformed = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
        malformed.write_i32(0);
        malformed.write_utf16("").unwrap();
        malformed.write_u8(0x51);
        assert!(matches!(
            dispatch_packet(&services, malformed.as_slice(), &mut context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes {
                    name: CHECK_PASSWORD_REQUEST_NAME,
                    count: 1,
                }
            ))
        ));
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        assert_eq!(
            world.authorize_identity(visitor.session).await.unwrap(),
            visitor.identity
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_myroom_reentry_and_random_stop_before_actor_publication() {
        let (world, actor) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_743),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "MalformedTypedEntry")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };

        for name in [
            REENTER_MYROOM_REQUEST_NAME,
            ENTER_RANDOM_MYROOM_REQUEST_NAME,
        ] {
            let mut malformed = PacketWriter::named(name).into_inner();
            malformed.push(0x51);
            assert!(matches!(
                dispatch_packet(&services, &malformed, &mut context).await,
                Err(LoginSessionError::MyRoomProtocol(
                    MyRoomProtocolError::TrailingBytes {
                        name: actual,
                        count: 1,
                    }
                )) if actual == name
            ));
            assert!(outbound.try_recv().is_err());
            assert_eq!(world.myroom_session_view(session).await.unwrap(), None);
        }

        shutdown_spawned_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn malformed_myroom_direct_entry_stops_before_actor_publication() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (session, _cancelled, mut outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_732),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let identity = world
            .claim_identity(session, "MalformedDirectEntry")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: session,
        };
        let mut malformed = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        malformed.write_utf16(&owner.identity.nickname).unwrap();
        malformed.write_utf16("").unwrap();
        let mut malformed = malformed.into_inner();
        malformed.push(0x51);

        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes { .. }
            ))
        ));
        assert!(outbound.try_recv().is_err());
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_direct_entry_backpressure_is_atomic_and_fails_session_explicitly() {
        let fixture = spawn_myroom_world_with_outbound_capacity(MyRoomInfo::default(), 1);
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (outsider_session, _cancelled, mut outsider_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_733),
                WireOperationGate::new(),
            )
            .await
            .unwrap();
        let outsider = world
            .claim_identity(outsider_session, "DirectEntryBackpressure")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut outsider_context = bind_test_profile(&profiles, &outsider).await;
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let outsider_services = SessionServices {
            session_id: outsider_session,
            ..owner_services
        };

        let queued_position = serialize_character_position(0, [51.36; 6]).unwrap();
        assert!(
            dispatch_packet(&owner_services, &queued_position, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        let mut enter = PacketWriter::named(ENTER_MYROOM_REQUEST_NAME);
        enter.write_utf16(&owner.identity.nickname).unwrap();
        enter.write_utf16("").unwrap();
        assert!(matches!(
            dispatch_packet(
                &outsider_services,
                &enter.into_inner(),
                &mut outsider_context,
            )
            .await,
            Err(LoginSessionError::World(
                WorldError::MyRoomCommandOutboundUnavailable { session }
            )) if session == visitor.session
        ));
        assert!(owner.outbound.try_recv().is_err());
        assert!(outsider_outbound.try_recv().is_err());
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![queued_position]
        );

        let first = PacketWriter::named("RmFirstRequestPacket").into_inner();
        assert!(
            dispatch_packet(&outsider_services, &first, &mut outsider_context)
                .await
                .unwrap()
                .is_empty()
        );
        let empty: [MyRoomSlot; MYROOM_SLOT_COUNT] = array::from_fn(|_| MyRoomSlot::Empty);
        assert_eq!(
            outsider_outbound.try_recv().unwrap().into_packets(),
            vec![serialize_slot_data(&empty).unwrap()],
            "entry backpressure must leave the requester outside every MyRoom"
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
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
        expected_slots[0] = MyRoomSlot::Player(
            myroom_profile_presentation(&owner_profile)
                .with_p2p_port(39_312)
                .player_for(&owner.identity),
        );
        expected_slots[1] = MyRoomSlot::Player(
            myroom_profile_presentation(&visitor_profile)
                .with_p2p_port(39_312)
                .player_for(&visitor.identity),
        );
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
        expected_slots[0] = MyRoomSlot::Player(
            myroom_profile_presentation(&owner_profile)
                .with_p2p_port(0)
                .player_for(&owner.identity),
        );
        expected_slots[1] = MyRoomSlot::Player(
            myroom_profile_presentation(&visitor_profile)
                .with_p2p_port(39_312)
                .player_for(&visitor.identity),
        );

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
        reason = "one end-to-end fixture proves validation, visitor preflight before profile capacity, durable three-slot persistence, and actor-owned ACK publication"
    )]
    async fn main_emblem_dispatch_fails_closed_and_persists_all_three_owner_slots() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let limits = crate::profile_io::ProfileIoLimits::for_tests(1, 1);
        let bootstrap =
            crate::profile_io::ProfileIoBootstrap::acquire(profile_root.path().to_owned(), limits)
                .unwrap();
        let (profile_io, profile_runtime) = bootstrap.spawn();
        let profiles = ProfileCoordinator::new(profile_io, Some(test_catalog()));
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let visitor_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };

        let mut unknown = PacketWriter::named("RmRqUpdateMainEmblemPacket");
        unknown.write_i16(7);
        unknown.write_i16(10);
        unknown.write_i16(9);
        assert_eq!(
            dispatch_packet(&owner_services, unknown.as_slice(), &mut owner_context)
                .await
                .unwrap(),
            vec![serialize_update_main_emblem_reply(false)]
        );
        let unchanged = store.load_or_create(&owner.identity.nickname).unwrap();
        assert_eq!(
            [
                unchanged.profile.rider.emblem1,
                unchanged.profile.rider.emblem2,
                unchanged.profile.rider.emblem3,
            ],
            [0, 0, 0]
        );
        assert!(owner.outbound.try_recv().is_err());

        let mut visitor_write = PacketWriter::named("RmRqUpdateMainEmblemPacket");
        visitor_write.write_i16(7);
        visitor_write.write_i16(8);
        visitor_write.write_i16(9);
        let held_profile_capacity = profiles
            .admit("MainEmblemCapacitySentinel", "hold profile capacity")
            .await
            .unwrap();
        assert_eq!(
            time::timeout(
                Duration::from_secs(1),
                dispatch_packet(
                    &visitor_services,
                    visitor_write.as_slice(),
                    &mut visitor_context,
                ),
            )
            .await
            .expect("a visitor must fail actor preflight before waiting for profile capacity")
            .unwrap(),
            vec![serialize_update_main_emblem_reply(false)]
        );
        drop(held_profile_capacity);
        assert!(visitor.outbound.try_recv().is_err());

        let mut valid = PacketWriter::named("RmRqUpdateMainEmblemPacket");
        valid.write_i16(7);
        valid.write_i16(8);
        valid.write_i16(9);
        assert!(
            dispatch_packet(&owner_services, valid.as_slice(), &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_update_main_emblem_reply(true)]
        );
        assert!(visitor.outbound.try_recv().is_err());
        let persisted = store.load_or_create(&owner.identity.nickname).unwrap();
        assert_eq!(
            [
                persisted.profile.rider.emblem1,
                persisted.profile.rider.emblem2,
                persisted.profile.rider.emblem3,
            ],
            [7, 8, 9]
        );
        let current = world.authorize_identity(owner.session).await.unwrap();
        assert_eq!(
            [
                owner_context.profile_for(&current).unwrap().rider.emblem1,
                owner_context.profile_for(&current).unwrap().rider.emblem2,
                owner_context.profile_for(&current).unwrap().rider.emblem3,
            ],
            [7, 8, 9]
        );

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_emblems_serializes_the_bounded_source_ordered_catalog() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let request = PacketWriter::named("RmRequestEmblemsPacket").into_inner();

        for (session_id, context, outbound) in [
            (owner.session, &mut owner_context, &mut owner.outbound),
            (visitor.session, &mut visitor_context, &mut visitor.outbound),
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
                    .is_empty()
            );
            assert_eq!(
                outbound.try_recv().unwrap().into_packets(),
                vec![serialize_owner_emblems(&[7, 8, 9]).unwrap()]
            );
        }

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn request_career_list_dispatches_only_the_terminal_empty_packet() {
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
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let request = PacketWriter::named(REQUEST_CAREER_LIST_NAME).into_inner();

        for (session_id, context, outbound) in [
            (owner.session, &mut owner_context, &mut owner.outbound),
            (visitor.session, &mut visitor_context, &mut visitor.outbound),
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
                    .is_empty()
            );
            assert_eq!(
                outbound.try_recv().unwrap().into_packets(),
                vec![serialize_empty_owner_career_list()]
            );
        }

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end fixture proves malformed input preserves the kind-two grant while successful and queue-full publications consume it exactly once"
    )]
    async fn protected_career_dispatch_is_strict_one_shot_and_burns_on_queue_full() {
        let fixture = spawn_myroom_world_with_outbound_capacity(
            MyRoomInfo {
                use_item_password: 1,
                item_password: "career dispatch secret".to_owned(),
                ..MyRoomInfo::default()
            },
            1,
        );
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let _owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut context = bind_test_profile(&profiles, &visitor.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };
        let career = PacketWriter::named(REQUEST_CAREER_LIST_NAME).into_inner();

        let mut password = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
        password.write_i32(2);
        password.write_utf16("career dispatch secret").unwrap();
        assert_eq!(
            dispatch_packet(&services, password.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_check_password_reply(
                2,
                CheckPasswordStatus::Success,
            )]
        );

        let mut malformed = career.clone();
        malformed.push(0x51);
        assert!(matches!(
            dispatch_packet(&services, &malformed, &mut context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes {
                    name: REQUEST_CAREER_LIST_NAME,
                    count: 1,
                }
            ))
        ));
        assert!(visitor.outbound.try_recv().is_err());

        assert!(
            dispatch_packet(&services, &career, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_empty_owner_career_list()]
        );
        assert!(
            dispatch_packet(&services, &career, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(visitor.outbound.try_recv().is_err());

        assert_eq!(
            dispatch_packet(&services, password.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_check_password_reply(
                2,
                CheckPasswordStatus::Success,
            )]
        );
        let first = PacketWriter::named(FIRST_MYROOM_REQUEST_NAME).into_inner();
        assert!(
            dispatch_packet(&services, &first, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            dispatch_packet(&services, &career, &mut context)
                .await
                .unwrap()
                .is_empty(),
            "Career queue saturation is a logged packet drop, not a session failure"
        );
        let queued = visitor.outbound.try_recv().unwrap().into_packets();
        assert_eq!(queued.len(), 1);
        assert_eq!(
            u32::from_le_bytes(queued[0][..4].try_into().unwrap()),
            adler32::packet_hash("RmSlotDataPacket")
        );
        assert!(
            dispatch_packet(&services, &career, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            visitor.outbound.try_recv().is_err(),
            "queue-full publication must not restore the consumed kind-two grant"
        );
        assert!(owner.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn missing_emblem_catalog_is_empty_and_allows_only_zero_selection() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            visitor: _visitor,
        } = fixture;
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let mut context = bind_test_profile(&profiles, &owner.identity).await;
        let config = ServerConfig::default();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };

        let request = PacketWriter::named("RmRequestEmblemsPacket").into_inner();
        assert!(
            dispatch_packet(&services, &request, &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_owner_emblems(&[]).unwrap()]
        );

        let mut unknown = PacketWriter::named("RmRqUpdateMainEmblemPacket");
        unknown.write_i16(7);
        unknown.write_i16(0);
        unknown.write_i16(0);
        assert_eq!(
            dispatch_packet(&services, unknown.as_slice(), &mut context)
                .await
                .unwrap(),
            vec![serialize_update_main_emblem_reply(false)]
        );
        assert!(owner.outbound.try_recv().is_err());

        let mut empty = PacketWriter::named("RmRqUpdateMainEmblemPacket");
        empty.write_i16(0);
        empty.write_i16(0);
        empty.write_i16(0);
        assert!(
            dispatch_packet(&services, empty.as_slice(), &mut context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_update_main_emblem_reply(true)]
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
    async fn myroom_character_position_relays_canonical_slots_without_sender_echo() {
        let fixture = spawn_myroom_world(MyRoomInfo::default());
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
            mut visitor,
        } = fixture;
        let (outsider_session, _cancelled, mut outsider_outbound) = world
            .register_login_session(
                SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_738),
                crate::operation_gate::WireOperationGate::new(),
            )
            .await
            .unwrap();
        let outsider_identity = world
            .claim_identity(outsider_session, "SessionMyRoomPositionOutsider")
            .await
            .unwrap();
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let mut owner_context = bind_test_profile(&profiles, &owner.identity).await;
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let mut outsider_context = bind_test_profile(&profiles, &outsider_identity).await;
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let visitor_services = SessionServices {
            session_id: visitor.session,
            ..owner_services
        };
        let outsider_services = SessionServices {
            session_id: outsider_session,
            ..owner_services
        };

        let owner_transform = [1.25, -2.5, 3.75, 4.0, -5.0, 6.0];
        let owner_position = serialize_character_position(0, owner_transform).unwrap();
        assert!(
            dispatch_packet(&owner_services, &owner_position, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![owner_position]
        );
        assert!(owner.outbound.try_recv().is_err());

        let visitor_transform = [-10.0, 20.0, -30.0, 40.0, -50.0, 60.0];
        let visitor_position = serialize_character_position(1, visitor_transform).unwrap();
        assert!(
            dispatch_packet(&visitor_services, &visitor_position, &mut visitor_context,)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![visitor_position]
        );
        assert!(visitor.outbound.try_recv().is_err());

        let spoofed = serialize_character_position(0, visitor_transform).unwrap();
        assert!(
            dispatch_packet(&visitor_services, &spoofed, &mut visitor_context)
                .await
                .unwrap()
                .is_empty(),
            "a valid but non-canonical sender slot is a silent drop"
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        let outsider_position = serialize_character_position(0, [7.0; 6]).unwrap();
        assert!(
            dispatch_packet(
                &outsider_services,
                &outsider_position,
                &mut outsider_context,
            )
            .await
            .unwrap()
            .is_empty(),
            "an authenticated nonmember cannot publish into a MyRoom"
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        assert!(outsider_outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_character_position_strictly_rejects_malformed_input_before_fanout() {
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
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };

        let valid = serialize_character_position(1, [1.0; 6]).unwrap();
        let mut trailing = valid.clone();
        trailing.push(0xa5);
        assert!(matches!(
            dispatch_packet(&services, &trailing, &mut visitor_context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes {
                    name: CHAR_POSITION_NAME,
                    count: 1,
                }
            ))
        ));
        assert!(matches!(
            dispatch_packet(&services, &valid[..valid.len() - 1], &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(_)
            ))
        ));

        let mut non_finite = PacketWriter::named(CHAR_POSITION_NAME);
        non_finite.write_i32(1);
        for value in [1.0, 2.0, f32::NAN, 4.0, 5.0, 6.0] {
            non_finite.write_f32(value);
        }
        assert!(matches!(
            dispatch_packet(&services, &non_finite.into_inner(), &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::NonFiniteTransform { index: 2 }
            ))
        ));

        let mut out_of_range = PacketWriter::named(CHAR_POSITION_NAME);
        out_of_range.write_i32(8);
        for value in [3.0; 6] {
            out_of_range.write_f32(value);
        }
        assert!(matches!(
            dispatch_packet(&services, &out_of_range.into_inner(), &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::InvalidSlot(8)
            ))
        ));

        let mut infinite = PacketWriter::named(CHAR_POSITION_NAME);
        infinite.write_i32(1);
        for value in [1.0, 2.0, 3.0, 4.0, f32::INFINITY, 6.0] {
            infinite.write_f32(value);
        }
        assert!(matches!(
            dispatch_packet(&services, &infinite.into_inner(), &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::NonFiniteTransform { index: 4 }
            ))
        ));

        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());
        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_character_position_backpressure_drops_the_whole_ephemeral_update() {
        let fixture = spawn_myroom_world_with_outbound_capacity(MyRoomInfo::default(), 1);
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
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let first = serialize_character_position(0, [1.0; 6]).unwrap();
        let second = serialize_character_position(0, [2.0; 6]).unwrap();

        assert!(
            dispatch_packet(&services, &first, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            dispatch_packet(&services, &second, &mut owner_context)
                .await
                .unwrap()
                .is_empty(),
            "ephemeral position backpressure drops the update without disconnecting its sender"
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![first],
            "the failed fanout cannot replace or append to the already queued batch"
        );
        assert!(visitor.outbound.try_recv().is_err());
        assert!(owner.outbound.try_recv().is_err());

        assert!(
            dispatch_packet(&services, &second, &mut owner_context)
                .await
                .unwrap()
                .is_empty(),
            "the same position is publishable after the bounded queue drains"
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![second]
        );
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_rider_talk_dispatches_canonical_echo_without_sender_echo() {
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
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let visitor_services = SessionServices {
            session_id: visitor.session,
            ..owner_services
        };

        let owner_talk = rider_talk_packet("owner says hello");
        assert!(
            dispatch_packet(&owner_services, &owner_talk, &mut owner_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            visitor.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_rider_echo(0, "owner says hello").unwrap()]
        );
        assert!(owner.outbound.try_recv().is_err());

        let maximum_message = "🏎".repeat(MAX_MYROOM_TALK_UTF16_UNITS / 2);
        let visitor_talk = rider_talk_packet(&maximum_message);
        assert!(
            dispatch_packet(&visitor_services, &visitor_talk, &mut visitor_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_rider_echo(1, &maximum_message).unwrap()]
        );
        assert!(visitor.outbound.try_recv().is_err());

        let empty_talk = rider_talk_packet("");
        assert!(
            dispatch_packet(&visitor_services, &empty_talk, &mut visitor_context)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_rider_echo(1, "").unwrap()]
        );
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_rider_talk_silently_drops_every_sender_when_talk_lock_is_zero() {
        let fixture = spawn_myroom_world(MyRoomInfo {
            talk_lock: 0,
            ..MyRoomInfo::default()
        });
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
        let owner_services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: owner.session,
        };
        let visitor_services = SessionServices {
            session_id: visitor.session,
            ..owner_services
        };

        assert!(
            dispatch_packet(
                &owner_services,
                &rider_talk_packet("owner bypass attempt"),
                &mut owner_context,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(
            dispatch_packet(
                &visitor_services,
                &rider_talk_packet("visitor bypass attempt"),
                &mut visitor_context,
            )
            .await
            .unwrap()
            .is_empty()
        );
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        shutdown_myroom_test(&world, profile_runtime, actor).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn myroom_rider_talk_rejects_malformed_input_before_actor_fanout() {
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
        let mut visitor_context = bind_test_profile(&profiles, &visitor.identity).await;
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id: visitor.session,
        };

        let valid = rider_talk_packet("still alive");
        let mut trailing = valid.clone();
        trailing.push(0xa5);
        assert!(matches!(
            dispatch_packet(&services, &trailing, &mut visitor_context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::TrailingBytes {
                    name: RIDER_TALK_NAME,
                    count: 1,
                }
            ))
        ));
        assert!(matches!(
            dispatch_packet(&services, &valid[..valid.len() - 1], &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(PacketError::Truncated { .. })
            ))
        ));

        let mut negative = PacketWriter::named(RIDER_TALK_NAME);
        negative.write_i32(-1);
        assert!(matches!(
            dispatch_packet(&services, negative.as_slice(), &mut visitor_context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(PacketError::NegativeStringLength(-1))
            ))
        ));

        let mut invalid_utf16 = PacketWriter::named(RIDER_TALK_NAME);
        invalid_utf16.write_i32(1);
        invalid_utf16.write_u16(0xd800);
        assert!(matches!(
            dispatch_packet(&services, invalid_utf16.as_slice(), &mut visitor_context,).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(PacketError::InvalidUtf16(_))
            ))
        ));

        let oversized_message = "x".repeat(MAX_MYROOM_TALK_UTF16_UNITS + 1);
        let oversized = rider_talk_packet(&oversized_message);
        assert!(matches!(
            dispatch_packet(&services, &oversized, &mut visitor_context).await,
            Err(LoginSessionError::MyRoomProtocol(
                MyRoomProtocolError::Packet(PacketError::StringLimitExceeded {
                    length,
                    maximum: MAX_MYROOM_TALK_UTF16_UNITS,
                })
            )) if length == MAX_MYROOM_TALK_UTF16_UNITS + 1
        ));
        assert!(owner.outbound.try_recv().is_err());
        assert!(visitor.outbound.try_recv().is_err());

        assert!(
            dispatch_packet(&services, &valid, &mut visitor_context)
                .await
                .unwrap()
                .is_empty(),
            "typed malformed input must not stop the actor or poison the bound session"
        );
        assert_eq!(
            owner.outbound.try_recv().unwrap().into_packets(),
            vec![serialize_rider_echo(1, "still alive").unwrap()]
        );
        assert!(visitor.outbound.try_recv().is_err());

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
    async fn item_password_success_grants_one_protected_owner_item_request() {
        let protected = MyRoomInfo {
            use_item_password: 1,
            item_password: "one shot item secret".to_owned(),
            ..MyRoomInfo::default()
        };
        let fixture = spawn_myroom_world(protected);
        let crate::world::test_support::MyRoomWorld {
            handle: world,
            actor,
            mut owner,
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
        let request_items = PacketWriter::named(REQUEST_MYROOM_ITEMS_NAME).into_inner();
        let expected_empty = serialize_owner_items(&[], &[], false).unwrap();

        for password_kind in [0, 3] {
            let mut check = PacketWriter::named(CHECK_PASSWORD_REQUEST_NAME);
            check.write_i32(password_kind);
            check.write_utf16("one shot item secret").unwrap();
            assert_eq!(
                dispatch_packet(&services, check.as_slice(), &mut visitor_context)
                    .await
                    .unwrap(),
                vec![serialize_check_password_reply(
                    password_kind,
                    CheckPasswordStatus::Success,
                )]
            );

            assert!(
                dispatch_packet(&services, &request_items, &mut visitor_context)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                visitor.outbound.try_recv().unwrap().into_packets(),
                expected_empty
            );

            assert!(
                dispatch_packet(&services, &request_items, &mut visitor_context)
                    .await
                    .unwrap()
                    .is_empty()
            );
            assert_eq!(
                visitor.outbound.try_recv().unwrap().into_packets(),
                vec![serialize_missing_owner_items()],
                "the password grant is consumed by exactly one owner-item request"
            );
            assert!(owner.outbound.try_recv().is_err());
            assert!(visitor.outbound.try_recv().is_err());
        }

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
    async fn game_slot_drops_are_nonfatal_and_preserve_followup_race_dispatch() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_765))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "GameSlotDrops")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = SessionContext::default();

        let truncated = PacketWriter::named(GAME_SLOT_PACKET_NAME).into_inner();
        let unsupported = unsupported_game_slot_packet(5);
        for packet in [truncated, unsupported] {
            assert_eq!(
                dispatch_packet(&services, &packet, &mut context)
                    .await
                    .unwrap(),
                Vec::<Vec<u8>>::new()
            );
        }

        assert_eq!(
            dispatch_packet(&services, &game_control_packet(0, 100), &mut context,)
                .await
                .unwrap(),
            Vec::<Vec<u8>>::new(),
            "a malformed GameSlot must not terminate the identity-bound session"
        );
        assert_eq!(
            world.authorize_identity(session_id).await.unwrap(),
            identity
        );

        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn valid_game_slot_no_room_and_pickups_have_no_direct_response() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let config = ServerConfig::default();
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session_id = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_766))
            .await
            .unwrap();
        let identity = world
            .claim_identity(session_id, "GameSlotNoRoom")
            .await
            .unwrap();
        let services = SessionServices {
            config: &config,
            world: &world,
            profiles: &profiles,
            session_id,
        };
        let mut context = SessionContext::default();

        for packet in [
            item_vector_game_slot_packet(),
            item_pickup_game_slot_packet(1),
            item_pickup_game_slot_packet(2),
        ] {
            assert_eq!(
                dispatch_packet(&services, &packet, &mut context)
                    .await
                    .unwrap(),
                Vec::<Vec<u8>>::new(),
                "TCP GameSlot relay and actor-routed pickup paths never return a direct packet"
            );
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
    async fn owned_x_part_request_persists_sidecar_before_exact_success() {
        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_801))
            .await
            .unwrap();
        let identity = world.claim_identity(session, "XPartOwner").await.unwrap();
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
        let request = XPartEquipRequest {
            kart_id: 1,
            kart_serial: 0,
            item_category: 68,
            item_id: 1_000,
            quantity: i16::MAX,
            unknown_1: 0,
            grade: 0,
            unknown_2: 0,
            parts_value: 0,
            unknown_3: 0,
        };
        let mut packet = PacketWriter::named("PqEquipXPartsItem");
        packet.write_i16(request.kart_id);
        packet.write_i16(request.kart_serial);
        packet.write_i16(request.item_category);
        packet.write_i16(request.item_id);
        packet.write_i16(request.quantity);
        packet.write_i16(request.unknown_1);
        packet.write_u8(request.grade);
        packet.write_u8(request.unknown_2);
        packet.write_i16(request.parts_value);
        packet.write_i16(request.unknown_3);

        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::EquipXPart,
            packet.as_slice(),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(responses, vec![serialize_equip_x_part_success(request)]);
        let equipment = EquipmentExceptions::load(profile_root.path(), rider_directory).unwrap();
        assert_eq!(equipment.parts.len(), 1);
        assert_eq!(equipment.parts[0].id, 1);
        assert_eq!(equipment.parts[0].serial, 1);
        assert_eq!(equipment.parts[0].coating, 1_000);

        world.session_closed(session).await.unwrap();
        world.shutdown().await.unwrap();
        world_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn generated_v1_x_part_is_accepted_and_invalid_value_is_non_terminal() {
        fn packet(request: XPartEquipRequest) -> Vec<u8> {
            let mut packet = PacketWriter::named("PqEquipXPartsItem");
            packet.write_i16(request.kart_id);
            packet.write_i16(request.kart_serial);
            packet.write_i16(request.item_category);
            packet.write_i16(request.item_id);
            packet.write_i16(request.quantity);
            packet.write_i16(request.unknown_1);
            packet.write_u8(request.grade);
            packet.write_u8(request.unknown_2);
            packet.write_i16(request.parts_value);
            packet.write_i16(request.unknown_3);
            packet.into_inner()
        }

        let profile_root = tempfile::tempdir().unwrap();
        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let (world, world_task) = WorldHandle::spawn(8).expect("nonzero World mailbox capacity");
        let session = world
            .register_session(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 49_802))
            .await
            .unwrap();
        let identity = world.claim_identity(session, "V1PartOwner").await.unwrap();
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

        let request = XPartEquipRequest {
            kart_id: 1,
            kart_serial: 1,
            item_category: 63,
            item_id: 2,
            quantity: i16::MAX,
            unknown_1: 0,
            grade: 2,
            unknown_2: 1,
            parts_value: 1_150,
            unknown_3: 0,
        };
        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::EquipXPart,
            &packet(request),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(responses, vec![serialize_equip_x_part_success(request)]);

        let equipment =
            EquipmentExceptions::load(profile_root.path(), rider_directory.clone()).unwrap();
        assert_eq!(equipment.parts.len(), 1);
        assert_eq!(equipment.parts[0].engine, 2);
        assert_eq!(equipment.parts[0].engine_grade, 2);
        assert_eq!(equipment.parts[0].engine_value, 1_150);

        let rejected = XPartEquipRequest {
            parts_value: 1_149,
            ..request
        };
        let responses = handle_equipment_request(
            &world,
            &profiles,
            session,
            EquipmentRequest::EquipXPart,
            &packet(rejected),
            &mut context,
        )
        .await
        .unwrap();
        assert_eq!(responses, vec![serialize_equip_x_part_failure(rejected)]);
        let equipment = EquipmentExceptions::load(profile_root.path(), rider_directory).unwrap();
        assert_eq!(equipment.parts[0].engine_value, 1_150);

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
    async fn get_rider_sanitizes_unresolved_equipped_kart_and_sidecar_records() {
        let profile_root = tempfile::tempdir().unwrap();
        let store = ProfileStore::new(profile_root.path());
        let mut profile = Profile::default();
        profile.rider_item.kart = 1_453;
        profile.rider_item.kart_serial = 1;
        let saved = store.save("UnsafeKartOwner", &profile).unwrap();
        let rider_directory = saved.path.parent().unwrap();
        fs::write(
            rider_directory.join("PlantData.json"),
            br#"[{"ID":1453,"SN":1,"Engine":43,"EngineID":1}]"#,
        )
        .unwrap();
        fs::write(
            rider_directory.join("PartsData.json"),
            br#"[{"ID":1453,"SN":1,"Engine":2,"EngineGrade":2,"EngineValue":1150}]"#,
        )
        .unwrap();

        let (profiles, _profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), Some(test_catalog()));
        let admission = profiles
            .admit("UnsafeKartOwner", "test unresolved kart sanitization")
            .await
            .unwrap();
        let (responses, snapshot, lane) = profiles
            .get_rider_sequence("UnsafeKartOwner".to_owned(), admission)
            .await
            .unwrap();
        drop(lane);

        assert_eq!(snapshot.profile.rider_item.kart, 0);
        assert_eq!(snapshot.profile.rider_item.kart_serial, 0);
        let item_hash = adler32::packet_hash("LoRpGetRiderItemPacket");
        let exception_hash = adler32::packet_hash("LoRpGetRiderExcDataPacket");
        let rider_hash = adler32::packet_hash("PrGetRider");
        let mut saw_rider = false;
        for response in &responses {
            let mut reader = PacketReader::new(response);
            match reader.read_u32().unwrap() {
                hash if hash == item_hash => {
                    reader.read_i32().unwrap();
                    reader.read_i32().unwrap();
                    let count = usize::try_from(reader.read_i32().unwrap()).unwrap();
                    for _ in 0..count {
                        let category = reader.read_u16().unwrap();
                        let item_id = reader.read_u16().unwrap();
                        assert_ne!(
                            (category, item_id),
                            (3, 1_453),
                            "unresolved kart leaked through the inventory stream"
                        );
                        reader.read_bytes(14).unwrap();
                    }
                    assert!(reader.remaining().is_empty());
                }
                hash if hash == exception_hash => {
                    panic!("unresolved kart leaked through an equipment exception packet")
                }
                hash if hash == rider_hash => {
                    saw_rider = true;
                    assert_eq!(reader.read_u8().unwrap(), 1);
                    assert_eq!(reader.read_u8().unwrap(), 0);
                    assert_eq!(reader.read_utf16().unwrap(), "UnsafeKartOwner");
                    for _ in 0..5 {
                        reader.read_u16().unwrap();
                    }
                    assert_eq!(
                        reader.read_bytes(RIDER_ITEM_SNAPSHOT_WIRE_LENGTH).unwrap(),
                        rider_item_snapshot(&snapshot.profile.rider_item)
                    );
                }
                _ => {}
            }
        }
        assert!(saw_rider);

        let persisted = store.reload("UnsafeKartOwner").unwrap();
        assert_eq!(persisted.profile.rider_item.kart, 0);
        assert_eq!(persisted.profile.rider_item.kart_serial, 0);
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
        assert_eq!(
            context.reported_p2p_port_for(&current).unwrap(),
            0,
            "a disk-side historical port must not replace this generation's runtime endpoint"
        );
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
        let cached_owner = myroom_profile_presentation(current_profile)
            .with_p2p_port(39_312)
            .player_for(&identity);

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
        ProfileStore::new(profile_root.path())
            .update(&identity.nickname, |profile| {
                profile.rider.p2p_port = 45_136;
            })
            .unwrap();

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
        assert_eq!(
            retry_context.profile_for(&migrated).unwrap().rider.p2p_port,
            45_136,
            "the historical durable value remains available in the profile snapshot"
        );
        assert_eq!(
            retry_context.reported_p2p_port_for(&migrated).unwrap(),
            0,
            "a migrated generation must begin without a live endpoint capability"
        );
        assert_eq!(
            retry_context
                .myroom_presentation_for(&migrated)
                .unwrap()
                .player_for(&migrated)
                .p2p_port,
            0,
            "MyRoom projection must use the generation-bound runtime port"
        );
        assert_eq!(
            retry_context
                .room_participant_for(&migrated, None)
                .unwrap()
                .player
                .p2p_port,
            0,
            "ordinary-room projection must use the generation-bound runtime port"
        );

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
    #[expect(
        clippy::too_many_lines,
        reason = "the regression spans accepted favorite persistence, request cancellation, identity drain, migration handoff, and durable verification"
    )]
    async fn cancelled_favorite_update_keeps_migration_fenced_until_durable() {
        let profile_root = tempfile::tempdir().unwrap();
        seed_canonical_favorite_profile(profile_root.path(), "FavoriteCancellation");
        let hook = BlockingUpdateHook::new();
        let (profiles, profile_runtime) =
            ProfileCoordinator::new_test(profile_root.path().to_owned(), None);
        let profiles = profiles.with_blocking_update_hook(Arc::clone(&hook));
        let (world, world_task) = WorldHandle::spawn(32).expect("nonzero World mailbox capacity");
        let address = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let source = world
            .register_session(SocketAddr::new(address, 50_010))
            .await
            .unwrap();
        let destination = world
            .register_session(SocketAddr::new(address, 50_011))
            .await
            .unwrap();
        let identity = world
            .claim_identity(source, "FavoriteCancellation")
            .await
            .unwrap();
        let admission = profiles
            .admit(&identity.nickname, "test initial favorite profile load")
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

        let update_profiles = profiles.clone();
        let update_nickname = identity.nickname.clone();
        let update_operation = world.admit_identity_operation(source).await.unwrap();
        let expected = FavoriteItemKey::new(3, 1_450, 7);
        let update = tokio::spawn(async move {
            let admission = update_profiles
                .admit_for_operation(
                    &update_operation,
                    &update_nickname,
                    FAVORITE_ITEM_UPDATE_OPERATION,
                )
                .await?;
            update_profiles
                .update_favorite_items(
                    update_nickname,
                    vec![FavoriteItemChange::new(
                        expected,
                        FavoriteItemOperation::Add,
                    )],
                    DEFAULT_MAX_FAVORITE_ITEM_LIST_RECORDS,
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
                .admit(&migration_nickname, "test favorite migration handoff")
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
            "migration acquired ownership while cancelled favorite persistence still ran"
        );

        let release_hook = Arc::clone(&hook);
        tokio::task::spawn_blocking(move || release_hook.release.wait())
            .await
            .unwrap();
        migration.await.unwrap().unwrap();

        let persisted = ProfileStore::new(profile_root.path())
            .reload(&identity.nickname)
            .unwrap();
        assert_eq!(
            favorite_item_snapshot(persisted.profile.favorite_items.as_ref()),
            [expected]
        );

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

        let stored_profile = ProfileStore::new(profile_root.path())
            .reload("PlantRider")
            .unwrap();
        let rider_directory = stored_profile
            .source_path
            .parent()
            .expect("the stored profile revision has a rider directory");
        let equipment = EquipmentExceptions::load(profile_root.path(), rider_directory).unwrap();
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
