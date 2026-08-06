//! Cross-platform P5136 server runtime.

mod client_catalog;
mod config;
mod equipment_persistence;
mod favorite_persistence;
mod identity;
mod item_probability;
mod locked_item_persistence;
mod main_emblem_persistence;
mod messenger_hub;
mod messenger_runtime;
mod myroom_hub;
mod myroom_persistence;
mod operation_gate;
mod packet_log;
mod profile_durability;
mod profile_io;
mod profile_presentation_persistence;
mod race_object_registry;
mod random_track;
mod runtime;
mod session;
mod udp_runtime;
mod udp_state;
mod world;

pub use client_catalog::{
    ClientKartCatalogError, ClientKartCatalogStats, LoadedClientKartCatalog,
    load_client_kart_catalog,
};
pub use config::{DEFAULT_MAX_LOGIN_SESSIONS, ServerConfig, ServerEndpoints};
pub use favorite_persistence::FavoriteItemPersistError;
pub use identity::{
    ChannelBinding, DisconnectOutcome, IdentityBinding, IdentityError, IdentityGeneration,
    IdentityRegistry, MIGRATION_TTL, MigrationCompletion, MigrationPermit, MigrationToken,
    ReleasedIdentity, UserNo,
};
pub use item_probability::{
    ItemProbabilityConfiguration, ItemProbabilityEntry, ItemProbabilityError,
    ItemProbabilityRankBand, ItemProbabilityRankPolicy, load_client_item_probabilities,
    load_item_probability_xml,
};
pub use locked_item_persistence::LockedItemPersistError;
pub use messenger_hub::{
    ChatClaim, EnterClaim, EnterOutcome, GenerationAdvance, GuildChatClaim, IdentityRelease,
    InviteClaim, LeaveClaim, MessengerDelivery, MessengerEvent, MessengerGeneration, MessengerHub,
    MessengerHubError, MessengerHubLimits, MessengerIdentity, MessengerRoomId, MessengerSessionId,
};
pub use messenger_runtime::{
    DEFAULT_MAX_MESSENGER_PAYLOAD, DEFAULT_MESSENGER_CONNECTION_CAPACITY,
    DEFAULT_MESSENGER_IDENTITY_CAPACITY, DEFAULT_MESSENGER_MAILBOX_CAPACITY,
    DEFAULT_MESSENGER_OUTBOUND_CAPACITY, MessengerCancellation, MessengerConnectionError,
    MessengerGenerationAdvanceOutcome, MessengerIdentityReleaseOutcome, MessengerRuntimeConfig,
    MessengerServiceError, MessengerServiceHandle, MessengerServiceSnapshot, read_messenger_frame,
};
pub use profile_io::{
    ProfileIoConfigError, ProfileIoError, ProfileIoRuntimeError, ProfileIoShutdownError,
};
pub use race_object_registry::{
    ItemOperationAuditDisposition, ItemOperationServerAudit, ItemOperationServerAuditError,
    audit_game_slot_item_operation,
};
pub use random_track::{
    RandomTrackCatalog, RandomTrackConfiguration, RandomTrackDefinition, RandomTrackError,
    RandomTrackPool, RandomTrackPoolOverride, ResolvedRandomTracks,
    load_client_random_track_catalog,
};
pub use runtime::{
    BoundServer, OperatorKartGrantError, RewardPersistenceRuntimeError, ServerError, ServerHandle,
};
pub use session::{LoginSessionError, read_encrypted_frame};
pub use udp_runtime::{
    DEFAULT_MAX_ACTIVE_UDP_IDENTITIES, DEFAULT_MAX_RELAY_TARGETS, DEFAULT_UDP_ADMISSION_CAPACITY,
    DEFAULT_UDP_COMMAND_CAPACITY, ServerClock, UdpDispatchAction, UdpDispatchOutcome,
    UdpDispatchRequest, UdpIngress, UdpIngressBody, UdpIngressDecodeError, UdpRuntime,
    UdpRuntimeConfig, UdpRuntimeEndpoints, UdpRuntimeEvent, UdpRuntimeFailure,
    UdpRuntimeStartError, UdpRuntimeStats, UdpRuntimeTask, UdpService, UdpServiceError,
    decode_udp_ingress,
};
pub use udp_state::{
    CurrentUdpEndpoint, UdpEndpointBindStatus, UdpEndpointBinding, UdpEndpointState,
    UdpEndpointStateError, UdpIngressBinding, UdpTransport,
};
pub use world::{
    OutstandingRewardLane, RaceFence, RewardDeadLetter, RewardDrainStatus, RewardLanePhase,
    RewardTerminalReason, RoomError, RoomId, RoomSnapshot, SessionId, SlotId, WorldActorError,
    WorldError, WorldHandle, WorldSpawnError,
};
