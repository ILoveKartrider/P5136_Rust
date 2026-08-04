//! Protocol-visible client state machine for the stock Korean P5136 client.
//!
//! This is a compatibility oracle, not a claim that every UI sub-state has
//! been recovered. It combines native consumer delegation with the packet
//! order from a known-working deployed trace. In particular, the native game
//! consumer can cache result packets, while this oracle deliberately enforces
//! the only ceremony order currently proven not to crash the stock client.

use std::{error::Error, fmt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Disconnected,
    AwaitingFirstMessage,
    EncryptedUnauthenticated,
    Authenticated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CeremonyPhase {
    AwaitingNextStage,
    AwaitingResult,
    Podium,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SceneState {
    Offline,
    Login,
    RiderBootstrap,
    Menu,
    Migration,
    RoomLobby,
    Loading,
    Racing,
    Settling,
    Ceremony(CeremonyPhase),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoomSnapshotState {
    pub session_data_seen: bool,
    pub slot_data_seen: bool,
}

impl RoomSnapshotState {
    #[must_use]
    pub const fn complete(self) -> bool {
        self.session_data_seen && self.slot_data_seen
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoadingState {
    pub game_control_zero_sent: bool,
    pub udp_time_sync_requested: bool,
    pub udp_time_sync_accepted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolState {
    pub transport: TransportState,
    pub scene: SceneState,
    pub migration_pending: bool,
    pub room_snapshot: RoomSnapshotState,
    pub loading: LoadingState,
    pub record_collection_flag: Option<bool>,
    pub local_finish_reported: bool,
}

impl Default for ProtocolState {
    fn default() -> Self {
        Self {
            transport: TransportState::Disconnected,
            scene: SceneState::Offline,
            migration_pending: false,
            room_snapshot: RoomSnapshotState::default(),
            loading: LoadingState::default(),
            record_collection_flag: None,
            local_finish_reported: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    ConnectionOpened,
    ConnectionClosed,
    ServerFirstMessage,
    ServerLoginAccepted,
    ServerRiderSnapshot,
    ServerChannelSwitch { reconnect: bool },
    ServerChannelMoveInAccepted,
    ServerRoomAdmissionAccepted,
    ServerRoomAdmissionRejected,
    ServerSessionData,
    ServerSlotData,
    ServerRoomMutation,
    ServerCommandStart,
    ClientGameControlZero,
    ClientUdpTimeSyncRequest,
    ServerUdpTimeSyncReply,
    ServerGameControlOne,
    ServerStartCollectRecord { flag: bool },
    ClientGameControlTwo,
    ServerRaceTime,
    ServerGameControlThree,
    ServerGameControlFour,
    ServerGameNextStage,
    ServerGameResult,
    ClientPodiumSchedulerCompleted,
    ServerLeaveRoom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolFsm {
    state: ProtocolState,
}

impl Default for ProtocolFsm {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolFsm {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: ProtocolState {
                transport: TransportState::Disconnected,
                scene: SceneState::Offline,
                migration_pending: false,
                room_snapshot: RoomSnapshotState {
                    session_data_seen: false,
                    slot_data_seen: false,
                },
                loading: LoadingState {
                    game_control_zero_sent: false,
                    udp_time_sync_requested: false,
                    udp_time_sync_accepted: false,
                },
                record_collection_flag: None,
                local_finish_reported: false,
            },
        }
    }

    #[must_use]
    pub const fn state(self) -> ProtocolState {
        self.state
    }

    pub fn accept(&mut self, event: Event) -> Result<ProtocolState, TransitionError> {
        let before = self.state;
        if self.apply(event) {
            Ok(self.state)
        } else {
            self.state = before;
            Err(TransitionError {
                event,
                transport: before.transport,
                scene: before.scene,
            })
        }
    }

    fn apply(&mut self, event: Event) -> bool {
        match event {
            Event::ConnectionOpened
            | Event::ConnectionClosed
            | Event::ServerFirstMessage
            | Event::ServerLoginAccepted
            | Event::ServerRiderSnapshot
            | Event::ServerChannelSwitch { .. }
            | Event::ServerChannelMoveInAccepted => self.apply_transport(event),
            Event::ServerRoomAdmissionAccepted
            | Event::ServerRoomAdmissionRejected
            | Event::ServerSessionData
            | Event::ServerSlotData
            | Event::ServerRoomMutation
            | Event::ServerCommandStart
            | Event::ServerLeaveRoom => self.apply_room(event),
            Event::ClientGameControlZero
            | Event::ClientUdpTimeSyncRequest
            | Event::ServerUdpTimeSyncReply
            | Event::ServerGameControlOne
            | Event::ServerStartCollectRecord { .. }
            | Event::ClientGameControlTwo
            | Event::ServerRaceTime
            | Event::ServerGameControlThree
            | Event::ServerGameControlFour
            | Event::ServerGameNextStage
            | Event::ServerGameResult
            | Event::ClientPodiumSchedulerCompleted => self.apply_race(event),
        }
    }

    fn apply_transport(&mut self, event: Event) -> bool {
        match event {
            Event::ConnectionOpened if self.state.transport == TransportState::Disconnected => {
                self.state.transport = TransportState::AwaitingFirstMessage;
                self.state.scene = if self.state.migration_pending {
                    SceneState::Migration
                } else {
                    SceneState::Login
                };
            }
            Event::ConnectionClosed => {
                self.state.transport = TransportState::Disconnected;
                self.state.scene = if self.state.migration_pending {
                    SceneState::Migration
                } else {
                    SceneState::Offline
                };
            }
            Event::ServerFirstMessage
                if self.state.transport == TransportState::AwaitingFirstMessage =>
            {
                self.state.transport = TransportState::EncryptedUnauthenticated;
            }
            Event::ServerLoginAccepted
                if self.state.transport == TransportState::EncryptedUnauthenticated
                    && !self.state.migration_pending =>
            {
                self.state.transport = TransportState::Authenticated;
                self.state.scene = SceneState::RiderBootstrap;
            }
            Event::ServerRiderSnapshot
                if self.state.transport == TransportState::Authenticated
                    && self.state.scene == SceneState::RiderBootstrap =>
            {
                self.state.scene = SceneState::Menu;
            }
            Event::ServerChannelSwitch { reconnect: true }
                if self.authenticated_in(SceneState::Menu) =>
            {
                self.state.migration_pending = true;
                self.state.scene = SceneState::Migration;
            }
            Event::ServerChannelSwitch { reconnect: false }
                if self.authenticated_in(SceneState::Menu) =>
            {
                // The mode-1 club UI hand-off does not migrate the socket.
                self.state.migration_pending = false;
            }
            Event::ServerChannelMoveInAccepted
                if self.state.transport == TransportState::EncryptedUnauthenticated
                    && self.state.migration_pending =>
            {
                self.state.transport = TransportState::Authenticated;
                self.state.scene = SceneState::Menu;
                self.state.migration_pending = false;
            }
            _ => return false,
        }
        true
    }

    fn apply_room(&mut self, event: Event) -> bool {
        match event {
            Event::ServerRoomAdmissionAccepted if self.authenticated_in(SceneState::Menu) => {
                self.state.scene = SceneState::RoomLobby;
                self.state.room_snapshot = RoomSnapshotState::default();
            }
            Event::ServerRoomAdmissionRejected if self.authenticated_in(SceneState::Menu) => {
                self.state.room_snapshot = RoomSnapshotState::default();
            }
            Event::ServerSessionData if self.authenticated_in(SceneState::RoomLobby) => {
                self.state.room_snapshot.session_data_seen = true;
            }
            Event::ServerSlotData if self.authenticated_in(SceneState::RoomLobby) => {
                self.state.room_snapshot.slot_data_seen = true;
            }
            Event::ServerRoomMutation if self.authenticated_in(SceneState::RoomLobby) => {}
            Event::ServerCommandStart if self.authenticated_in(SceneState::RoomLobby) => {
                // GrCommandStart carries fresh nested session and slot packets,
                // so prior standalone snapshots are not a native guard.
                self.state.scene = SceneState::Loading;
                self.state.room_snapshot = RoomSnapshotState {
                    session_data_seen: true,
                    slot_data_seen: true,
                };
                self.state.loading = LoadingState::default();
                self.state.record_collection_flag = None;
                self.state.local_finish_reported = false;
            }
            Event::ServerLeaveRoom
                if self.state.transport == TransportState::Authenticated
                    && matches!(
                        self.state.scene,
                        SceneState::RoomLobby
                            | SceneState::Loading
                            | SceneState::Racing
                            | SceneState::Settling
                            | SceneState::Ceremony(_)
                    ) =>
            {
                self.state.scene = SceneState::Menu;
                self.state.room_snapshot = RoomSnapshotState::default();
                self.state.loading = LoadingState::default();
                self.state.record_collection_flag = None;
                self.state.local_finish_reported = false;
            }
            _ => return false,
        }
        true
    }

    fn apply_race(&mut self, event: Event) -> bool {
        match event {
            Event::ClientGameControlZero if self.authenticated_in(SceneState::Loading) => {
                self.state.loading.game_control_zero_sent = true;
            }
            Event::ClientUdpTimeSyncRequest if self.authenticated_in(SceneState::Loading) => {
                self.state.loading.udp_time_sync_requested = true;
            }
            Event::ServerUdpTimeSyncReply
                if self.authenticated_in(SceneState::Loading)
                    && self.state.loading.udp_time_sync_requested =>
            {
                self.state.loading.udp_time_sync_accepted = true;
            }
            Event::ServerGameControlOne if self.authenticated_in(SceneState::Loading) => {
                // A server timeout can start a client that never originated
                // UDP time-sync, so synchronization is evidence, not a guard.
                self.state.scene = SceneState::Racing;
            }
            Event::ServerStartCollectRecord { flag }
                if self.state.transport == TransportState::Authenticated
                    && matches!(
                        self.state.scene,
                        SceneState::Loading | SceneState::Racing | SceneState::Settling
                    ) =>
            {
                self.state.record_collection_flag = Some(flag);
            }
            Event::ClientGameControlTwo if self.authenticated_in(SceneState::Racing) => {
                self.state.local_finish_reported = true;
            }
            Event::ServerRaceTime if self.racing_or_settling() => {}
            Event::ServerGameControlThree if self.authenticated_in(SceneState::Racing) => {
                self.state.scene = SceneState::Settling;
            }
            Event::ServerGameControlFour if self.authenticated_in(SceneState::Settling) => {
                self.state.scene = SceneState::Ceremony(CeremonyPhase::AwaitingNextStage);
            }
            Event::ServerGameNextStage
                if self
                    .authenticated_in(SceneState::Ceremony(CeremonyPhase::AwaitingNextStage)) =>
            {
                self.state.scene = SceneState::Ceremony(CeremonyPhase::AwaitingResult);
            }
            Event::ServerGameResult
                if self.authenticated_in(SceneState::Ceremony(CeremonyPhase::AwaitingResult)) =>
            {
                self.state.scene = SceneState::Ceremony(CeremonyPhase::Podium);
            }
            Event::ClientPodiumSchedulerCompleted
                if self.authenticated_in(SceneState::Ceremony(CeremonyPhase::Podium)) =>
            {
                self.return_to_room();
            }
            _ => return false,
        }
        true
    }

    fn authenticated_in(&self, scene: SceneState) -> bool {
        self.state.transport == TransportState::Authenticated && self.state.scene == scene
    }

    fn racing_or_settling(&self) -> bool {
        self.state.transport == TransportState::Authenticated
            && matches!(self.state.scene, SceneState::Racing | SceneState::Settling)
    }

    fn return_to_room(&mut self) {
        self.state.scene = SceneState::RoomLobby;
        self.state.room_snapshot = RoomSnapshotState {
            session_data_seen: true,
            slot_data_seen: true,
        };
        self.state.loading = LoadingState::default();
        self.state.record_collection_flag = None;
        self.state.local_finish_reported = false;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransitionError {
    pub event: Event,
    pub transport: TransportState,
    pub scene: SceneState,
}

impl fmt::Display for TransitionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "event {:?} is invalid in transport {:?}, scene {:?}",
            self.event, self.transport, self.scene
        )
    }
}

impl Error for TransitionError {}
