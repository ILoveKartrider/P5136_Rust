use p5136_client_oracle::protocol_fsm::{
    CeremonyPhase, Event, ProtocolFsm, SceneState, TransportState,
};

fn enter_menu(fsm: &mut ProtocolFsm) {
    for event in [
        Event::ConnectionOpened,
        Event::ServerFirstMessage,
        Event::ServerLoginAccepted,
        Event::ServerRiderSnapshot,
    ] {
        fsm.accept(event).unwrap();
    }
}

fn enter_room(fsm: &mut ProtocolFsm) {
    enter_menu(fsm);
    fsm.accept(Event::ServerRoomAdmissionAccepted).unwrap();
    fsm.accept(Event::ServerSessionData).unwrap();
    fsm.accept(Event::ServerSlotData).unwrap();
}

#[test]
fn cold_login_reaches_menu_only_after_the_rider_snapshot() {
    let mut fsm = ProtocolFsm::new();
    fsm.accept(Event::ConnectionOpened).unwrap();
    assert_eq!(fsm.state().scene, SceneState::Login);
    assert_eq!(fsm.state().transport, TransportState::AwaitingFirstMessage);

    fsm.accept(Event::ServerFirstMessage).unwrap();
    fsm.accept(Event::ServerLoginAccepted).unwrap();
    assert_eq!(fsm.state().scene, SceneState::RiderBootstrap);
    assert!(fsm.accept(Event::ServerRoomAdmissionAccepted).is_err());

    fsm.accept(Event::ServerRiderSnapshot).unwrap();
    assert_eq!(fsm.state().scene, SceneState::Menu);
}

#[test]
fn normal_channel_switch_preserves_migration_across_reconnect() {
    let mut fsm = ProtocolFsm::new();
    enter_menu(&mut fsm);
    fsm.accept(Event::ServerChannelSwitch { reconnect: true })
        .unwrap();
    fsm.accept(Event::ConnectionClosed).unwrap();
    assert_eq!(fsm.state().scene, SceneState::Migration);
    assert!(fsm.state().migration_pending);

    fsm.accept(Event::ConnectionOpened).unwrap();
    fsm.accept(Event::ServerFirstMessage).unwrap();
    fsm.accept(Event::ServerChannelMoveInAccepted).unwrap();
    assert_eq!(fsm.state().transport, TransportState::Authenticated);
    assert_eq!(fsm.state().scene, SceneState::Menu);
    assert!(!fsm.state().migration_pending);
}

#[test]
fn club_ui_channel_switch_does_not_force_a_transport_migration() {
    let mut fsm = ProtocolFsm::new();
    enter_menu(&mut fsm);
    fsm.accept(Event::ServerChannelSwitch { reconnect: false })
        .unwrap();
    assert_eq!(fsm.state().transport, TransportState::Authenticated);
    assert_eq!(fsm.state().scene, SceneState::Menu);
    assert!(!fsm.state().migration_pending);
}

#[test]
fn command_start_is_self_contained_and_does_not_require_standalone_snapshots() {
    let mut fsm = ProtocolFsm::new();
    enter_menu(&mut fsm);
    fsm.accept(Event::ServerRoomAdmissionAccepted).unwrap();
    assert!(!fsm.state().room_snapshot.complete());

    fsm.accept(Event::ServerCommandStart).unwrap();
    assert_eq!(fsm.state().scene, SceneState::Loading);
    assert!(fsm.state().room_snapshot.complete());
}

#[test]
fn udp_readiness_is_observed_but_timeout_start_remains_legal() {
    let mut fsm = ProtocolFsm::new();
    enter_room(&mut fsm);
    fsm.accept(Event::ServerCommandStart).unwrap();
    fsm.accept(Event::ServerGameControlOne).unwrap();
    assert_eq!(fsm.state().scene, SceneState::Racing);

    let mut synchronized = ProtocolFsm::new();
    enter_room(&mut synchronized);
    synchronized.accept(Event::ServerCommandStart).unwrap();
    synchronized.accept(Event::ClientGameControlZero).unwrap();
    synchronized
        .accept(Event::ClientUdpTimeSyncRequest)
        .unwrap();
    synchronized.accept(Event::ServerUdpTimeSyncReply).unwrap();
    assert!(synchronized.state().loading.game_control_zero_sent);
    assert!(synchronized.state().loading.udp_time_sync_accepted);
    synchronized.accept(Event::ServerGameControlOne).unwrap();
}

#[test]
fn start_collect_record_reply_is_a_guarded_race_side_effect_not_a_scene_transition() {
    let mut fsm = ProtocolFsm::new();
    enter_room(&mut fsm);
    assert!(
        fsm.accept(Event::ServerStartCollectRecord { flag: true })
            .is_err()
    );

    fsm.accept(Event::ServerCommandStart).unwrap();
    let before_scene = fsm.state().scene;
    fsm.accept(Event::ServerStartCollectRecord { flag: true })
        .unwrap();
    assert_eq!(fsm.state().scene, before_scene);
    assert_eq!(fsm.state().record_collection_flag, Some(true));

    fsm.accept(Event::ServerGameControlOne).unwrap();
    fsm.accept(Event::ServerStartCollectRecord { flag: false })
        .unwrap();
    assert_eq!(fsm.state().scene, SceneState::Racing);
    assert_eq!(fsm.state().record_collection_flag, Some(false));

    fsm.accept(Event::ServerGameControlThree).unwrap();
    fsm.accept(Event::ServerStartCollectRecord { flag: true })
        .unwrap();
    assert_eq!(fsm.state().record_collection_flag, Some(true));

    fsm.accept(Event::ServerGameControlFour).unwrap();
    assert!(
        fsm.accept(Event::ServerStartCollectRecord { flag: false })
            .is_err()
    );
    assert_eq!(fsm.state().record_collection_flag, Some(true));
}

#[test]
fn race_and_ceremony_follow_the_deployed_compatibility_order() {
    let mut fsm = ProtocolFsm::new();
    enter_room(&mut fsm);
    for event in [
        Event::ServerCommandStart,
        Event::ServerGameControlOne,
        Event::ClientGameControlTwo,
        Event::ServerRaceTime,
        Event::ServerGameControlThree,
        Event::ServerGameControlFour,
        Event::ServerGameNextStage,
        Event::ServerGameResult,
    ] {
        fsm.accept(event).unwrap();
    }
    assert_eq!(
        fsm.state().scene,
        SceneState::Ceremony(CeremonyPhase::Podium)
    );

    fsm.accept(Event::ClientPodiumSchedulerCompleted).unwrap();
    assert_eq!(fsm.state().scene, SceneState::RoomLobby);
    assert!(fsm.state().room_snapshot.complete());
}

#[test]
fn result_packets_and_lobby_snapshots_cannot_cross_scene_boundaries() {
    let mut fsm = ProtocolFsm::new();
    enter_room(&mut fsm);
    fsm.accept(Event::ServerCommandStart).unwrap();
    fsm.accept(Event::ServerGameControlOne).unwrap();
    fsm.accept(Event::ServerGameControlThree).unwrap();

    assert!(fsm.accept(Event::ServerGameNextStage).is_err());
    fsm.accept(Event::ServerGameControlFour).unwrap();
    assert!(fsm.accept(Event::ServerGameResult).is_err());
    assert!(fsm.accept(Event::ServerSlotData).is_err());
    fsm.accept(Event::ServerGameNextStage).unwrap();
    fsm.accept(Event::ServerGameResult).unwrap();
}

#[test]
fn leave_room_is_a_cross_phase_escape_but_disconnect_resets_the_scene() {
    for leave_at in [
        SceneState::RoomLobby,
        SceneState::Loading,
        SceneState::Racing,
        SceneState::Settling,
        SceneState::Ceremony(CeremonyPhase::AwaitingNextStage),
    ] {
        let mut fsm = ProtocolFsm::new();
        enter_room(&mut fsm);
        if leave_at != SceneState::RoomLobby {
            fsm.accept(Event::ServerCommandStart).unwrap();
        }
        if matches!(
            leave_at,
            SceneState::Racing | SceneState::Settling | SceneState::Ceremony(_)
        ) {
            fsm.accept(Event::ServerGameControlOne).unwrap();
        }
        if matches!(leave_at, SceneState::Settling | SceneState::Ceremony(_)) {
            fsm.accept(Event::ServerGameControlThree).unwrap();
        }
        if matches!(leave_at, SceneState::Ceremony(_)) {
            fsm.accept(Event::ServerGameControlFour).unwrap();
        }
        assert_eq!(fsm.state().scene, leave_at);
        fsm.accept(Event::ServerLeaveRoom).unwrap();
        assert_eq!(fsm.state().scene, SceneState::Menu);
    }

    let mut disconnected = ProtocolFsm::new();
    enter_room(&mut disconnected);
    disconnected.accept(Event::ConnectionClosed).unwrap();
    assert_eq!(disconnected.state().transport, TransportState::Disconnected);
    assert_eq!(disconnected.state().scene, SceneState::Offline);
}

#[test]
fn rejected_transition_is_transactional() {
    let mut fsm = ProtocolFsm::new();
    enter_menu(&mut fsm);
    let before = fsm.state();
    assert!(fsm.accept(Event::ServerGameControlFour).is_err());
    assert_eq!(fsm.state(), before);
}
