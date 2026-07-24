//! Cross-platform P5136 server runtime.

mod config;
mod identity;
mod runtime;
mod session;
mod world;

pub use config::{DEFAULT_MAX_LOGIN_SESSIONS, ServerConfig, ServerEndpoints};
pub use identity::{
    ChannelBinding, DisconnectOutcome, IdentityBinding, IdentityError, IdentityGeneration,
    IdentityRegistry, MIGRATION_TTL, MigrationCompletion, MigrationPermit, MigrationToken,
    ReleasedIdentity, UserNo,
};
pub use runtime::{BoundServer, ServerError, ServerHandle};
pub use session::{LoginSessionError, read_encrypted_frame};
pub use world::{RoomError, RoomId, RoomSnapshot, SessionId, SlotId, WorldError, WorldHandle};
