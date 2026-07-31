//! Pure modern-P5136 UDP endpoint binding state.
//!
//! A world actor which owns the identity registry can call
//! [`UdpEndpointState::bind_ingress`]. The standalone socket service instead
//! accepts a caller-resolved identity through
//! [`UdpEndpointState::bind_authorized_ingress`]. Both paths perform source-IP
//! authorization and generation-bound endpoint mutation synchronously, leaving
//! no authorization/update `await` boundary.

use std::{
    collections::HashMap,
    fmt,
    net::{IpAddr, SocketAddr},
    num::NonZeroUsize,
};

use thiserror::Error;

use crate::{IdentityBinding, IdentityGeneration, IdentityRegistry, ReleasedIdentity, UserNo};

pub const DEFAULT_MAXIMUM_ACTIVE_UDP_IDENTITIES: usize = 4_096;

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

    #[error("UDP active identity capacity {maximum} is exhausted")]
    ActiveIdentityCapacity { maximum: usize },

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

    #[error(
        "{transport} account ID {account_id} UDP arrival epoch {arrival_epoch} does not follow reconnect boundary {boundary_epoch}"
    )]
    IngressPredatesReconnect {
        transport: UdpTransport,
        account_id: u32,
        arrival_epoch: u64,
        boundary_epoch: u64,
    },
}

#[derive(Debug)]
pub struct UdpEndpointState {
    active: HashMap<UserNo, ActiveIdentity>,
    maximum_active_identities: NonZeroUsize,
    game: GenerationEndpointTable,
    p2p: GenerationEndpointTable,
}

impl Default for UdpEndpointState {
    fn default() -> Self {
        Self::with_max_active_identities(
            NonZeroUsize::new(DEFAULT_MAXIMUM_ACTIVE_UDP_IDENTITIES)
                .expect("the default UDP identity capacity is non-zero"),
        )
    }
}

impl UdpEndpointState {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    #[must_use]
    pub fn with_max_active_identities(maximum_active_identities: NonZeroUsize) -> Self {
        Self {
            active: HashMap::new(),
            maximum_active_identities,
            game: GenerationEndpointTable::default(),
            p2p: GenerationEndpointTable::default(),
        }
    }

    #[must_use]
    pub fn active_identity_count(&self) -> usize {
        self.active.len()
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
        self.advance_identity(&identity)?;
        self.bind_authorized_ingress(transport, &identity, source, route_hash)
    }

    /// Binds ingress after the service caller has resolved the active identity.
    ///
    /// Keeping this operation synchronous lets an actor resolve an identity,
    /// pass its exact generation, and mutate endpoint state without performing
    /// socket I/O inside the authorization boundary.
    pub fn bind_authorized_ingress(
        &mut self,
        transport: UdpTransport,
        identity: &IdentityBinding,
        source: SocketAddr,
        route_hash: u32,
    ) -> Result<UdpIngressBinding, UdpEndpointStateError> {
        self.bind_authorized_ingress_at(transport, identity, source, route_hash, u64::MAX)
    }

    /// Production ingress variant which also enforces any same-generation
    /// reconnect boundary published by the TCP control plane.
    pub fn bind_authorized_ingress_at(
        &mut self,
        transport: UdpTransport,
        identity: &IdentityBinding,
        source: SocketAddr,
        route_hash: u32,
        arrival_epoch: u64,
    ) -> Result<UdpIngressBinding, UdpEndpointStateError> {
        let account_id = identity.user_no.get();
        let user_no = identity.user_no;
        if source.port() == 0 {
            return Err(UdpEndpointStateError::InvalidSourceEndpoint { endpoint: source });
        }
        let active = self
            .active
            .get(&user_no)
            .copied()
            .ok_or(UdpEndpointStateError::InactiveAccount { account_id })?;
        if active.generation != identity.generation {
            return Err(UdpEndpointStateError::StaleGeneration {
                transport,
                account_id,
                attempted_generation: identity.generation.get(),
                current_generation: active.generation.get(),
            });
        }
        if identity.source_ip != active.source_ip {
            return Err(UdpEndpointStateError::SourceIpMismatch {
                account_id,
                expected: active.source_ip,
                received: identity.source_ip,
            });
        }
        if let Some(boundary_epoch) = active.reconnect_boundary_epoch
            && arrival_epoch <= boundary_epoch
        {
            return Err(UdpEndpointStateError::IngressPredatesReconnect {
                transport,
                account_id,
                arrival_epoch,
                boundary_epoch,
            });
        }
        if source.ip() != active.source_ip {
            return Err(UdpEndpointStateError::SourceIpMismatch {
                account_id,
                expected: active.source_ip,
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
            identity: identity.clone(),
            endpoint,
            status,
        })
    }

    /// Clears both endpoints for one exact active generation and installs an
    /// arrival-epoch barrier. A datagram received before the TCP reconnect
    /// report cannot race the actor queue and reclaim the endpoint afterward.
    pub fn authorize_rebind(
        &mut self,
        identity: &IdentityBinding,
        boundary_epoch: u64,
    ) -> Result<(), UdpEndpointStateError> {
        let account_id = identity.user_no.get();
        let active = self
            .active
            .get_mut(&identity.user_no)
            .ok_or(UdpEndpointStateError::InactiveAccount { account_id })?;
        if active.generation != identity.generation {
            return Err(UdpEndpointStateError::StaleGeneration {
                transport: UdpTransport::Game,
                account_id,
                attempted_generation: identity.generation.get(),
                current_generation: active.generation.get(),
            });
        }
        if active.source_ip != identity.source_ip {
            return Err(UdpEndpointStateError::SourceIpMismatch {
                account_id,
                expected: active.source_ip,
                received: identity.source_ip,
            });
        }
        if active
            .reconnect_boundary_epoch
            .is_some_and(|current| boundary_epoch <= current)
        {
            return Ok(());
        }
        active.reconnect_boundary_epoch = Some(boundary_epoch);
        self.game.release(identity.user_no, identity.generation);
        self.p2p.release(identity.user_no, identity.generation);
        Ok(())
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
        self.current_authorized_target(transport, &identity)
    }

    /// Resolves a target only for the exact identity generation supplied by an
    /// authorization owner such as the world actor.
    #[must_use]
    pub fn current_authorized_target(
        &self,
        transport: UdpTransport,
        identity: &IdentityBinding,
    ) -> Option<CurrentUdpEndpoint> {
        let active = self.active.get(&identity.user_no)?;
        if active.generation != identity.generation || active.source_ip != identity.source_ip {
            return None;
        }
        let endpoint = self
            .table(transport)
            .get_authorized(identity.user_no, identity.generation)?;
        if endpoint.generation != identity.generation
            || endpoint.endpoint.ip() != identity.source_ip
        {
            return None;
        }
        Some(CurrentUdpEndpoint {
            identity: identity.clone(),
            endpoint,
        })
    }

    /// Activates or advances the exact generation mirror without inventing an
    /// endpoint.
    ///
    /// A stale advance is ignored. Adding a distinct active account is bounded
    /// by the configured capacity; advancing an existing account is always
    /// allowed.
    pub fn advance_identity(
        &mut self,
        identity: &IdentityBinding,
    ) -> Result<(), UdpEndpointStateError> {
        if let Some(current) = self.active.get(&identity.user_no).copied() {
            if current.generation.get() > identity.generation.get() {
                return Ok(());
            }
            if current.generation == identity.generation {
                if current.source_ip != identity.source_ip {
                    return Err(UdpEndpointStateError::SourceIpMismatch {
                        account_id: identity.user_no.get(),
                        expected: current.source_ip,
                        received: identity.source_ip,
                    });
                }
                return Ok(());
            }
        }

        if !self.active.contains_key(&identity.user_no)
            && self.active.len() >= self.maximum_active_identities.get()
        {
            return Err(UdpEndpointStateError::ActiveIdentityCapacity {
                maximum: self.maximum_active_identities.get(),
            });
        }
        self.active
            .insert(identity.user_no, ActiveIdentity::from(identity));
        Ok(())
    }

    /// Removes both transport routes only when the released generation is
    /// current. A delayed release for an older owner cannot erase a replacement
    /// owner's endpoint.
    pub fn remove_released_identity(&mut self, identity: &ReleasedIdentity) {
        if !self
            .active
            .get(&identity.user_no)
            .is_some_and(|active| active.generation == identity.generation)
        {
            return;
        }
        self.active.remove(&identity.user_no);
        self.game.release(identity.user_no, identity.generation);
        self.p2p.release(identity.user_no, identity.generation);
    }

    pub fn clear(&mut self) {
        self.active.clear();
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveIdentity {
    generation: IdentityGeneration,
    source_ip: IpAddr,
    reconnect_boundary_epoch: Option<u64>,
}

impl From<&IdentityBinding> for ActiveIdentity {
    fn from(identity: &IdentityBinding) -> Self {
        Self {
            generation: identity.generation,
            source_ip: identity.source_ip,
            reconnect_boundary_epoch: None,
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
                    current: current.generation,
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

    fn get_authorized(
        &self,
        user_no: UserNo,
        generation: IdentityGeneration,
    ) -> Option<UdpEndpointBinding> {
        self.bindings
            .get(&user_no)
            .copied()
            .filter(|binding| binding.generation == generation)
    }

    fn release(&mut self, user_no: UserNo, generation: IdentityGeneration) {
        if self
            .bindings
            .get(&user_no)
            .is_some_and(|binding| binding.generation == generation)
        {
            self.bindings.remove(&user_no);
        }
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
        current: IdentityGeneration,
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
                current_generation: current.get(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        net::{IpAddr, Ipv4Addr, SocketAddr},
        num::NonZeroUsize,
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
        endpoints.advance_identity(&destination).unwrap();
        assert!(
            endpoints
                .current_authorized_target(UdpTransport::Game, &source)
                .is_none(),
            "the explicit generation fence must reject a stale caller"
        );
        assert_eq!(
            endpoints.bind_authorized_ingress(UdpTransport::Game, &source, GAME_ENDPOINT, 2,),
            Err(UdpEndpointStateError::StaleGeneration {
                transport: UdpTransport::Game,
                account_id: source.user_no.get(),
                attempted_generation: source.generation.get(),
                current_generation: destination.generation.get(),
            })
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
                current: advanced.endpoint.generation,
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
    fn release_and_advance_are_exact_generation_fences() {
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
        assert_eq!(endpoints.active_identity_count(), 0);
        assert!(endpoints.game.bindings.is_empty());
        assert!(endpoints.p2p.bindings.is_empty());
        assert_eq!(
            endpoints.bind_authorized_ingress(UdpTransport::Game, &identity, GAME_ENDPOINT, 3,),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: identity.user_no.get(),
            })
        );
        assert!(
            endpoints
                .current_target(&identities, UdpTransport::Game, identity.user_no)
                .is_none()
        );

        let replacement = identities
            .claim(SessionId::new(2), SOURCE_IP, "rIDER")
            .unwrap();
        endpoints.advance_identity(&replacement).unwrap();
        let advanced = endpoints
            .bind_authorized_ingress(UdpTransport::Game, &replacement, GAME_ALTERNATE, 4)
            .unwrap();
        assert_eq!(advanced.status, UdpEndpointBindStatus::Bound);

        endpoints.remove_released_identity(&released);
        assert_eq!(
            endpoints
                .current_target(&identities, UdpTransport::Game, replacement.user_no)
                .unwrap()
                .endpoint,
            advanced.endpoint,
            "a delayed old-generation release cannot remove a replacement"
        );

        let DisconnectOutcome::Released(replacement_release) =
            identities.disconnect(SessionId::new(2), now)
        else {
            panic!("replacement owner did not release");
        };
        endpoints.remove_released_identity(&replacement_release);
        assert_eq!(
            endpoints.bind_authorized_ingress(UdpTransport::Game, &replacement, GAME_ALTERNATE, 5,),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: replacement.user_no.get(),
            })
        );
        assert_eq!(endpoints.active_identity_count(), 0);
        assert!(endpoints.game.bindings.is_empty());
        assert!(endpoints.p2p.bindings.is_empty());
    }

    #[test]
    fn active_identity_capacity_is_finite_and_replacement_does_not_consume_another_slot() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let source = identities
            .claim(SessionId::new(1), SOURCE_IP, "Rider")
            .unwrap();
        let mut endpoints =
            UdpEndpointState::with_max_active_identities(NonZeroUsize::new(1).unwrap());
        endpoints.advance_identity(&source).unwrap();

        let permit = identities
            .begin_migration(SessionId::new(1), CHANNEL, token(300), now)
            .unwrap();
        let replacement = identities
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
        endpoints.advance_identity(&replacement).unwrap();
        assert_eq!(endpoints.active_identity_count(), 1);

        let rival = identities
            .claim(SessionId::new(3), SOURCE_IP, "Rival")
            .unwrap();
        assert_eq!(
            endpoints.advance_identity(&rival),
            Err(UdpEndpointStateError::ActiveIdentityCapacity { maximum: 1 })
        );
        assert_eq!(endpoints.active_identity_count(), 1);
        assert_eq!(
            endpoints.bind_authorized_ingress(UdpTransport::Game, &rival, GAME_ENDPOINT, 0,),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: rival.user_no.get(),
            })
        );
    }

    #[test]
    fn retained_identity_and_endpoint_are_rejected_after_exact_release() {
        let now = Instant::now();
        let (mut identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        let retained = endpoints
            .bind_ingress(
                &identities,
                UdpTransport::Game,
                identity.user_no.get(),
                GAME_ENDPOINT,
                7,
            )
            .unwrap();
        let DisconnectOutcome::Released(released) = identities.disconnect(SessionId::new(1), now)
        else {
            panic!("current owner did not release");
        };
        endpoints.remove_released_identity(&released);

        assert!(
            endpoints
                .current_authorized_target(UdpTransport::Game, &retained.identity)
                .is_none()
        );
        assert_eq!(
            endpoints.bind_authorized_ingress(
                UdpTransport::Game,
                &retained.identity,
                retained.endpoint.endpoint,
                retained.endpoint.route_hash,
            ),
            Err(UdpEndpointStateError::InactiveAccount {
                account_id: identity.user_no.get(),
            })
        );
        assert_eq!(endpoints.active_identity_count(), 0);
        assert!(endpoints.game.bindings.is_empty());
        assert!(endpoints.p2p.bindings.is_empty());
    }

    #[test]
    fn sequential_identity_churn_leaves_no_mirror_routes_or_tombstones() {
        let now = Instant::now();
        let mut identities = IdentityRegistry::new();
        let mut endpoints =
            UdpEndpointState::with_max_active_identities(NonZeroUsize::new(1).unwrap());

        for session in 1..=512 {
            let identity = identities
                .claim(SessionId::new(session), SOURCE_IP, "Rider")
                .unwrap();
            let route_hash = u32::try_from(session).unwrap();
            endpoints.advance_identity(&identity).unwrap();
            endpoints
                .bind_authorized_ingress(UdpTransport::Game, &identity, GAME_ENDPOINT, route_hash)
                .unwrap();
            endpoints
                .bind_authorized_ingress(UdpTransport::P2p, &identity, P2P_ENDPOINT, route_hash)
                .unwrap();

            let DisconnectOutcome::Released(released) =
                identities.disconnect(SessionId::new(session), now)
            else {
                panic!("current owner did not release");
            };
            endpoints.remove_released_identity(&released);

            assert_eq!(endpoints.active_identity_count(), 0);
            assert!(endpoints.active.is_empty());
            assert!(endpoints.game.bindings.is_empty());
            assert!(endpoints.p2p.bindings.is_empty());
        }
    }

    #[test]
    fn fresh_identity_accepts_epoch_zero_until_a_reconnect_boundary_is_published() {
        let (_identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints.advance_identity(&identity).unwrap();

        let initial = endpoints
            .bind_authorized_ingress_at(UdpTransport::Game, &identity, GAME_ENDPOINT, 1, 0)
            .unwrap();
        assert_eq!(initial.status, UdpEndpointBindStatus::Bound);

        endpoints.authorize_rebind(&identity, 0).unwrap();
        assert_eq!(
            endpoints.bind_authorized_ingress_at(
                UdpTransport::Game,
                &identity,
                GAME_ENDPOINT,
                2,
                0,
            ),
            Err(UdpEndpointStateError::IngressPredatesReconnect {
                transport: UdpTransport::Game,
                account_id: identity.user_no.get(),
                arrival_epoch: 0,
                boundary_epoch: 0,
            })
        );
    }

    #[test]
    fn reconnect_boundary_clears_both_routes_and_rejects_pre_report_arrivals() {
        let (_identities, identity) = active_identity();
        let mut endpoints = UdpEndpointState::new();
        endpoints.advance_identity(&identity).unwrap();
        endpoints
            .bind_authorized_ingress_at(UdpTransport::Game, &identity, GAME_ENDPOINT, 1, 10)
            .unwrap();
        endpoints
            .bind_authorized_ingress_at(UdpTransport::P2p, &identity, P2P_ENDPOINT, 2, 11)
            .unwrap();

        endpoints.authorize_rebind(&identity, 20).unwrap();
        assert!(
            endpoints
                .current_authorized_target(UdpTransport::Game, &identity)
                .is_none()
        );
        assert!(
            endpoints
                .current_authorized_target(UdpTransport::P2p, &identity)
                .is_none()
        );
        assert_eq!(
            endpoints.bind_authorized_ingress_at(
                UdpTransport::Game,
                &identity,
                GAME_ENDPOINT,
                3,
                20,
            ),
            Err(UdpEndpointStateError::IngressPredatesReconnect {
                transport: UdpTransport::Game,
                account_id: identity.user_no.get(),
                arrival_epoch: 20,
                boundary_epoch: 20,
            })
        );
        let rebound = endpoints
            .bind_authorized_ingress_at(UdpTransport::Game, &identity, GAME_ALTERNATE, 4, 21)
            .unwrap();
        assert_eq!(rebound.status, UdpEndpointBindStatus::Bound);
        assert_eq!(rebound.endpoint.endpoint, GAME_ALTERNATE);
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
