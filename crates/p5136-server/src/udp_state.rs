//! Pure modern-P5136 UDP endpoint binding state.
//!
//! The future world actor can call [`UdpEndpointState::bind_ingress`] while it
//! owns both this value and the [`IdentityRegistry`]. The method performs the
//! account lookup, source-IP authorization, and generation-bound endpoint
//! update synchronously, leaving no authorization/update `await` boundary.
//! Socket I/O and relay audience selection deliberately live elsewhere.

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, SocketAddr},
};

use thiserror::Error;

use crate::{IdentityBinding, IdentityGeneration, IdentityRegistry, ReleasedIdentity, UserNo};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UdpTransport {
    Game,
    P2p,
}

impl fmt::Display for UdpTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Game => "game UDP",
            Self::P2p => "P2P UDP",
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpEndpointBinding {
    pub endpoint: SocketAddr,
    pub route_hash: u32,
    pub generation: IdentityGeneration,
    /// Reserved for verified client-reported direct-P2P state. The initial
    /// integration deliberately leaves this false and relays through the
    /// server.
    pub direct_p2p: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdpEndpointBindStatus {
    Bound,
    Refreshed,
    AdvancedGeneration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpIngressBinding {
    pub identity: IdentityBinding,
    pub endpoint: UdpEndpointBinding,
    pub status: UdpEndpointBindStatus,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentUdpEndpoint {
    pub identity: IdentityBinding,
    pub endpoint: UdpEndpointBinding,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum UdpEndpointStateError {
    #[error("UDP account ID zero is invalid")]
    ZeroAccountId,

    #[error("UDP account ID {account_id} has no active current-generation owner")]
    InactiveAccount { account_id: u32 },

    #[error("UDP source endpoint {endpoint} has invalid port zero")]
    InvalidSourceEndpoint { endpoint: SocketAddr },

    #[error(
        "UDP account ID {account_id} source IP mismatch: expected {expected}, received {received}"
    )]
    SourceIpMismatch {
        account_id: u32,
        expected: IpAddr,
        received: IpAddr,
    },

    #[error(
        "{transport} account ID {account_id} generation {generation} is already bound to {bound}; rejected {attempted}"
    )]
    EndpointMismatch {
        transport: UdpTransport,
        account_id: u32,
        generation: u64,
        bound: SocketAddr,
        attempted: SocketAddr,
    },

    #[error(
        "{transport} account ID {account_id} stale generation {attempted_generation}; current endpoint generation is {current_generation}"
    )]
    StaleGeneration {
        transport: UdpTransport,
        account_id: u32,
        attempted_generation: u64,
        current_generation: u64,
    },
}

#[derive(Debug, Default)]
pub struct UdpEndpointState {
    game: GenerationEndpointTable,
    p2p: GenerationEndpointTable,
}

impl UdpEndpointState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Authorizes and binds one decrypted, validated UDP ingress header.
    ///
    /// Endpoint ownership follows modern `UdpServer.cs`: the first endpoint
    /// wins within a generation; that endpoint may refresh its opaque route
    /// hash; a different endpoint is accepted only after identity generation
    /// advancement.
    pub fn bind_ingress(
        &mut self,
        identities: &IdentityRegistry,
        transport: UdpTransport,
        account_id: u32,
        source: SocketAddr,
        route_hash: u32,
    ) -> Result<UdpIngressBinding, UdpEndpointStateError> {
        let user_no = UserNo::new(account_id).ok_or(UdpEndpointStateError::ZeroAccountId)?;
        let identity = identities
            .active_identity_by_user_no(user_no)
            .ok_or(UdpEndpointStateError::InactiveAccount { account_id })?;
        if source.port() == 0 {
            return Err(UdpEndpointStateError::InvalidSourceEndpoint { endpoint: source });
        }
        if source.ip() != identity.source_ip {
            return Err(UdpEndpointStateError::SourceIpMismatch {
                account_id,
                expected: identity.source_ip,
                received: source.ip(),
            });
        }

        let candidate = UdpEndpointBinding {
            endpoint: source,
            route_hash,
            generation: identity.generation,
            direct_p2p: false,
        };
        let (status, endpoint) = self
            .table_mut(transport)
            .bind(user_no, candidate)
            .map_err(|error| map_table_error(transport, user_no, source, error))?;
        Ok(UdpIngressBinding {
            identity,
            endpoint,
            status,
        })
    }

    /// Returns a target only when both its identity and endpoint belong to the
    /// same currently owned generation and source IP.
    #[must_use]
    pub fn current_target(
        &self,
        identities: &IdentityRegistry,
        transport: UdpTransport,
        user_no: UserNo,
    ) -> Option<CurrentUdpEndpoint> {
        let identity = identities.active_identity_by_user_no(user_no)?;
        let endpoint = self.table(transport).get(user_no)?;
        if endpoint.generation != identity.generation
            || endpoint.endpoint.ip() != identity.source_ip
        {
            return None;
        }
        Some(CurrentUdpEndpoint { identity, endpoint })
    }

    /// Removes both transport routes after the world actor releases an
    /// identity. User numbers are stable and never reused by the registry.
    pub fn remove_released_identity(&mut self, identity: &ReleasedIdentity) {
        self.game.remove(identity.user_no);
        self.p2p.remove(identity.user_no);
    }

    pub fn clear(&mut self) {
        self.game.clear();
        self.p2p.clear();
    }

    fn table(&self, transport: UdpTransport) -> &GenerationEndpointTable {
        match transport {
            UdpTransport::Game => &self.game,
            UdpTransport::P2p => &self.p2p,
        }
    }

    fn table_mut(&mut self, transport: UdpTransport) -> &mut GenerationEndpointTable {
        match transport {
            UdpTransport::Game => &mut self.game,
            UdpTransport::P2p => &mut self.p2p,
        }
    }
}

#[derive(Debug, Default)]
struct GenerationEndpointTable {
    bindings: HashMap<UserNo, UdpEndpointBinding>,
}

impl GenerationEndpointTable {
    fn bind(
        &mut self,
        user_no: UserNo,
        candidate: UdpEndpointBinding,
    ) -> Result<(UdpEndpointBindStatus, UdpEndpointBinding), TableBindError> {
        let status = match self.bindings.get(&user_no).copied() {
            None => UdpEndpointBindStatus::Bound,
            Some(current) if current.generation.get() > candidate.generation.get() => {
                return Err(TableBindError::StaleGeneration {
                    current,
                    attempted: candidate.generation,
                });
            }
            Some(current)
                if current.generation == candidate.generation
                    && current.endpoint != candidate.endpoint =>
            {
                return Err(TableBindError::EndpointMismatch { current });
            }
            Some(current) if current.generation == candidate.generation => {
                UdpEndpointBindStatus::Refreshed
            }
            Some(_) => UdpEndpointBindStatus::AdvancedGeneration,
        };
        self.bindings.insert(user_no, candidate);
        Ok((status, candidate))
    }

    fn get(&self, user_no: UserNo) -> Option<UdpEndpointBinding> {
        self.bindings.get(&user_no).copied()
    }

    fn remove(&mut self, user_no: UserNo) {
        self.bindings.remove(&user_no);
    }

    fn clear(&mut self) {
        self.bindings.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TableBindError {
    EndpointMismatch {
        current: UdpEndpointBinding,
    },
    StaleGeneration {
        current: UdpEndpointBinding,
        attempted: IdentityGeneration,
    },
}

fn map_table_error(
    transport: UdpTransport,
    user_no: UserNo,
    attempted: SocketAddr,
    error: TableBindError,
) -> UdpEndpointStateError {
    match error {
        TableBindError::EndpointMismatch { current } => UdpEndpointStateError::EndpointMismatch {
            transport,
            account_id: user_no.get(),
            generation: current.generation.get(),
            bound: current.endpoint,
            attempted,
        },
        TableBindError::StaleGeneration { current, attempted } => {
            UdpEndpointStateError::StaleGeneration {
                transport,
                account_id: user_no.get(),
                attempted_generation: attempted.get(),
                current_generation: current.generation.get(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        time::Instant,
    };

    use super::{
        TableBindError, UdpEndpointBindStatus, UdpEndpointBinding, UdpEndpointState,
        UdpEndpointStateError, UdpTransport,
    };
    use crate::{ChannelBinding, DisconnectOutcome, IdentityRegistry, MigrationToken, SessionId};

    const SOURCE_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10));
    const OTHER_IP: IpAddr = IpAddr::V4(Ipv4Addr::new(192, 0, 2, 11));
    const GAME_ENDPOINT: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 51_000);
    const GAME_ALTERNATE: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 51_001);
    const P2P_ENDPOINT: SocketAddr =
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 0, 2, 10)), 52_000);
    const CHANNEL: ChannelBinding = ChannelBinding {
        channel_id: 11,
        game_type: 67,
    };

    #[test]
    fn first_bind_and_same_endpoint_refresh_preserve_the_modern_fields() {
        let (identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        let first = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                0,
            )
            .unwrap();
        assert_eq!(first.status, UdpEndpointBindStatus::Bound);
        assert_eq!(first.identity, identity);
        assert_eq!(first.endpoint.endpoint, GAME_ENDPOINT);
        assert_eq!(first.endpoint.route_hash, 0);
        assert_eq!(first.endpoint.generation, identity.generation);
        assert!(!first.endpoint.direct_p2p);

        let refreshed = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                u32::MAX,
            )
            .unwrap();
        assert_eq!(refreshed.status, UdpEndpointBindStatus::Refreshed);
        assert_eq!(refreshed.endpoint.route_hash, u32::MAX);
        assert_eq!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .unwrap()
                .endpoint,
            refreshed.endpoint
        );
    }

    #[test]
    fn same_generation_alternate_endpoint_is_rejected_without_replacement() {
        let (identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                0x1111_1111,
            )
            .unwrap();

        assert_eq!(
            endpoints.bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ALTERNATE,
                0x2222_2222,
            ),
            Err(UdpEndpointStateError::EndpointMismatch {
                transport: UdpTransport::Game,
                account_id: identity.user_no.get(),
                generation: identity.generation.get(),
                bound: GAME_ENDPOINT,
                attempted: GAME_ALTERNATE,
            })
        );
        let current = endpoints
            .current_target(&identities, UdpTransport::Game, identity.user_no)
            .unwrap();
        assert_eq!(current.endpoint.endpoint, GAME_ENDPOINT);
        assert_eq!(current.endpoint.route_hash, 0x1111_1111);
    }

    #[test]
    fn generation_advance_replaces_the_endpoint_and_stale_bind_is_rejected() {
        let now = Instant::now();
        let (mut identities, source) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                source.user_no.get(),
                GAME_ENDPOINT,
                1,
            )
            .unwrap();
        let old_generation = source.generation;

        let permit = identities
            .begin_migration(SessionId::new(1), CHANNEL, token(100), now)
            .unwrap();
        let destination = identities
            .complete_migration(
                SessionId::new(2),
                SOURCE_IP,
                source.user_no,
                CHANNEL.channel_id,
                permit.token,
                now,
            )
            .unwrap()
            .binding;
        assert!(
            endpoints
                .current_target(&identities, UdpTransport::Game, source.user_no)
                .is_none(),
            "a previous-generation route must not be returned as a target"
        );

        let advanced = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                source.user_no.get(),
                GAME_ALTERNATE,
                2,
            )
            .unwrap();
        assert_eq!(advanced.status, UdpEndpointBindStatus::AdvancedGeneration);
        assert_eq!(advanced.identity, destination);
        assert_eq!(advanced.endpoint.endpoint, GAME_ALTERNATE);

        let stale = UdpEndpointBinding {
            endpoint: GAME_ENDPOINT,
            route_hash: 3,
            generation: old_generation,
            direct_p2p: false,
        };
        assert_eq!(
            endpoints.game.bind(source.user_no, stale),
            Err(TableBindError::StaleGeneration {
                current: advanced.endpoint,
                attempted: old_generation,
            })
        );
        assert_eq!(
            endpoints
                .current_target(&identities, UdpTransport::Game, source.user_no)
                .unwrap()
                .endpoint,
            advanced.endpoint
        );
    }

    #[test]
    fn game_and_p2p_transport_tables_are_independent() {
        let (identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        let game = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                0x1111_1111,
            )
            .unwrap();
        let p2p = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::P2p,
                identity.user_no.get(),
                P2P_ENDPOINT,
                0x2222_2222,
            )
            .unwrap();
        assert_eq!(game.status, UdpEndpointBindStatus::Bound);
        assert_eq!(p2p.status, UdpEndpointBindStatus::Bound);
        assert_eq!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .unwrap()
                .endpoint,
            game.endpoint
        );
        assert_eq!(
            endpoints
                .current_target(&identities, UdpTransport::P2p, identity.user_no)
                .unwrap()
                .endpoint,
            p2p.endpoint
        );
    }

    #[test]
    fn zero_unknown_wrong_ip_and_zero_port_sources_are_rejected_before_binding() {
        let (identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        assert_eq!(
            endpoints.bind_ingress(&identities, UdpTransport::Game, 0, GAME_ENDPOINT, 0,),
            Err(UdpEndpointStateError::ZeroAccountId)
        );
        assert_eq!(
            endpoints.bind_ingress(&identities, UdpTransport::Game, u32::MAX, GAME_ENDPOINT, 0,),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: u32::MAX,
            })
        );
        let wrong_ip = SocketAddr::new(OTHER_IP, 51_000);
        assert_eq!(
            endpoints.bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                wrong_ip,
                0,
            ),
            Err(UdpEndpointStateError::SourceIpMismatch {
                account_id: identity.user_no.get(),
                expected: SOURCE_IP,
                received: OTHER_IP,
            })
        );
        let zero_port = SocketAddr::new(SOURCE_IP, 0);
        assert_eq!(
            endpoints.bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                zero_port,
                0,
            ),
            Err(UdpEndpointStateError::InvalidSourceEndpoint {
                endpoint: zero_port,
            })
        );
        assert!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .is_none()
        );
    }

    #[test]
    fn ownerless_migration_generation_cannot_bind_or_be_targeted() {
        let now = Instant::now();
        let (mut identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                0,
            )
            .unwrap();
        identities
            .begin_migration(SessionId::new(1), CHANNEL, token(200), now)
            .unwrap();
        assert!(matches!(
            identities.disconnect(SessionId::new(1), now),
            DisconnectOutcome::Deferred { .. }
        ));

        assert_eq!(
            endpoints.bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                0,
            ),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: identity.user_no.get(),
            })
        );
        assert!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .is_none()
        );
    }

    #[test]
    fn released_identity_cleanup_removes_both_transport_routes() {
        let now = Instant::now();
        let (mut identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                1,
            )
            .unwrap();
        endpoints
            .bind_ingress(
                &identities,
                UdpTransport::P2p,
                identity.user_no.get(),
                P2P_ENDPOINT,
                2,
            )
            .unwrap();

        let DisconnectOutcome::Released(released) = identities.disconnect(SessionId::new(1), now)
        else {
            panic!("current owner did not release");
        };
        endpoints.remove_released_identity(&released);
        assert!(endpoints.game.bindings.is_empty());
        assert!(endpoints.p2p.bindings.is_empty());
        assert!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .is_none()
        );
    }

    fn active_identity() -> (IdentityRegistry, crate::IdentityBinding) {
        let mut identities = IdentityRegistry::new();
        let identity = identities
            .claim(SessionId::new(1), SOURCE_IP, "Rider")
            .unwrap();
        (identities, identity)
    }

    fn token(value: u16) -> MigrationToken {
        MigrationToken::new(value).unwrap()
    }
}
