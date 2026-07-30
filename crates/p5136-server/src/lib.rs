//! Cross-platform P5136 server runtime.

mod config;
mod equipment_persistence;
mod favorite_persistence;
mod identity;
mod main_emblem_persistence;
mod messenger_hub;
mod messenger_runtime;
mod myroom_hub;
mod myroom_persistence;
mod operation_gate;
mod profile_durability;
mod profile_io;
mod profile_presentation_persistence;
mod runtime;
mod session;
mod udp_runtime;
mod udp_state;
mod world;

pub use config::{DEFAULT_MAX_LOGIN_SESSIONS, ServerConfig, ServerEndpoints};
pub use favorite_persistence::FavoriteItemPersistError;
pub use identity::{
    ChannelBinding, DisconnectOutcome, IdentityBinding, IdentityError, IdentityGeneration,
    IdentityRegistry, MIGRATION_TTL, MigrationCompletion, MigrationPermit, MigrationToken,
    ReleasedIdentity, UserNo,
};
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
pub use runtime::{BoundServer, RewardPersistenceRuntimeError, ServerError, ServerHandle};
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
