//! Cross-platform P5136 server runtime.

mod config;
mod identity;
mod messenger_hub;
mod runtime;
mod session;
mod udp_state;
mod world;

pub use config::{DEFAULT_MAX_LOGIN_SESSIONS, ServerConfig, ServerEndpoints};
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
pub use runtime::{BoundServer, ServerError, ServerHandle};
pub use session::{LoginSessionError, read_encrypted_frame};
pub use udp_state::{
    CurrentUdpEndpoint, UdpEndpointBindStatus, UdpEndpointBinding, UdpEndpointState,
    UdpEndpointStateError, UdpIngressBinding, UdpTransport,
};
pub use world::{RoomError, RoomId, RoomSnapshot, SessionId, SlotId, WorldError, WorldHandle};
